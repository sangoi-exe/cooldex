//! Session-wide mutable state.

use std::collections::BTreeSet;
use std::collections::HashMap;

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::codex::SessionConfiguration;
use crate::context_manager::ContextManager;
use crate::instructions::SkillInstructions;
use crate::instructions::UserInstructions;
use crate::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use crate::protocol::REASONING_CONTEXT_CLOSE_TAG;
use crate::protocol::REASONING_CONTEXT_OPEN_TAG;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::TOOL_CONTEXT_OPEN_TAG;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::rid::rid_to_string;
use crate::state::ContextItemSummary;
use crate::state::ContextItemsEvent;
use crate::state::ContextOverlay;
use crate::state::PruneCategory;
use crate::truncate::TruncationPolicy;

/// Persistent, session-scoped state previously stored directly on `Session`.
pub(crate) struct SessionState {
    pub(crate) session_configuration: SessionConfiguration,
    pub(crate) history: ContextManager,
    context_inclusion_mask: Option<BTreeSet<u64>>,
    context_overlay: ContextOverlay,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) dependency_env: HashMap<String, String>,
    pub(crate) mcp_dependency_prompted: HashSet<String>,
    /// Whether the session's initial context has been seeded into history.
    ///
    /// TODO(owen): This is a temporary solution to avoid updating a thread's updated_at
    /// timestamp when resuming a session. Remove this once SQLite is in place.
    pub(crate) initial_context_seeded: bool,
    /// Previous rollout model for one-shot model-switch handling on first turn after resume.
    pub(crate) pending_resume_previous_model: Option<String>,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new(session_configuration: SessionConfiguration) -> Self {
        let history = ContextManager::new();
        Self {
            session_configuration,
            history,
            context_inclusion_mask: None,
            context_overlay: ContextOverlay::default(),
            latest_rate_limits: None,
            server_reasoning_included: false,
            dependency_env: HashMap::new(),
            mcp_dependency_prompted: HashSet::new(),
            initial_context_seeded: false,
            pending_resume_previous_model: None,
        }
    }

    // History helpers
    pub(crate) fn record_items<I>(&mut self, items: I, policy: TruncationPolicy)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        let before_rids_len = self.history.rids_len();
        self.history.record_items(items, policy);
        let Some(mask) = self.context_inclusion_mask.as_mut() else {
            return;
        };

        let (all_items, all_rids) = self.history.items_with_rids();
        let before = before_rids_len.min(all_items.len());
        let (prev_items, new_items) = all_items.split_at(before);
        let (prev_rids, new_rids) = all_rids.split_at(before);
        let mut call_included: HashMap<String, bool> = HashMap::new();
        for (item, rid) in prev_items.iter().zip(prev_rids.iter()) {
            if let Some(call_id) = call_id_for_call_item(item) {
                call_included.insert(call_id.to_string(), mask.contains(rid));
            }
        }
        for (item, rid) in prev_items.iter().zip(prev_rids.iter()) {
            if let Some(call_id) = call_id_for_output_item(item) {
                call_included
                    .entry(call_id.to_string())
                    .or_insert(mask.contains(rid));
            }
        }

        for (item, rid) in new_items.iter().zip(new_rids.iter()) {
            let include = if let Some(call_id) = call_id_for_tool_item(item) {
                *call_included.entry(call_id.to_string()).or_insert(true)
            } else {
                true
            };

            if include {
                mask.insert(*rid);
            } else {
                mask.remove(rid);
            }
        }
    }

    pub(crate) fn clone_history(&self) -> ContextManager {
        self.history.clone()
    }

    pub(crate) fn replace_history(&mut self, items: Vec<ResponseItem>) {
        self.history.replace(items);
        self.context_inclusion_mask = None;
        self.context_overlay.replacements_by_rid.clear();
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.history.set_token_info(info);
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<i64>,
    ) {
        self.history.update_token_info(usage, model_context_window);
    }

    pub(crate) fn token_info(&self) -> Option<TokenUsageInfo> {
        self.history.token_info()
    }

    pub(crate) fn set_rate_limits(&mut self, snapshot: RateLimitSnapshot) {
        self.latest_rate_limits = Some(merge_rate_limit_fields(
            self.latest_rate_limits.as_ref(),
            snapshot,
        ));
    }

    pub(crate) fn token_info_and_rate_limits(
        &self,
    ) -> (Option<TokenUsageInfo>, Option<RateLimitSnapshot>) {
        (self.token_info(), self.latest_rate_limits.clone())
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: i64) {
        self.history.set_token_usage_full(context_window);
    }

    pub(crate) fn get_total_token_usage(&self, server_reasoning_included: bool) -> i64 {
        self.history
            .get_total_token_usage(server_reasoning_included)
    }

    pub(crate) fn set_server_reasoning_included(&mut self, included: bool) {
        self.server_reasoning_included = included;
    }

    pub(crate) fn server_reasoning_included(&self) -> bool {
        self.server_reasoning_included
    }

    pub(crate) fn record_mcp_dependency_prompted<I>(&mut self, names: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.mcp_dependency_prompted.extend(names);
    }

    pub(crate) fn mcp_dependency_prompted(&self) -> HashSet<String> {
        self.mcp_dependency_prompted.clone()
    }

    pub(crate) fn set_dependency_env(&mut self, values: HashMap<String, String>) {
        for (key, value) in values {
            self.dependency_env.insert(key, value);
        }
    }

    pub(crate) fn dependency_env(&self) -> HashMap<String, String> {
        self.dependency_env.clone()
    }

    pub(crate) fn history_snapshot(&self) -> Vec<ResponseItem> {
        let mut history = self.history.clone();
        history.get_history_for_prompt()
    }

    pub(crate) fn history_snapshot_lenient(&self) -> Vec<ResponseItem> {
        self.history.get_history_for_prompt_lenient()
    }

    pub(crate) fn history_rids_snapshot(&self) -> Vec<u64> {
        let mut history = self.history.clone();
        history.get_history_for_prompt_with_rids().1
    }

    pub(crate) fn history_rids_snapshot_lenient(&self) -> Vec<u64> {
        self.history.get_history_for_prompt_with_rids_lenient().1
    }

    pub(crate) fn history_snapshot_with_rids_lenient(&self) -> (Vec<ResponseItem>, Vec<u64>) {
        self.history.get_history_for_prompt_with_rids_lenient()
    }

    pub(crate) fn context_overlay_snapshot(&self) -> ContextOverlay {
        self.context_overlay.clone()
    }

    pub(crate) fn set_context_overlay(&mut self, overlay: ContextOverlay) {
        self.context_overlay = overlay;
    }

    pub(crate) fn add_context_notes(&mut self, notes: Vec<String>) {
        for note in notes {
            let trimmed = note.trim();
            if trimmed.is_empty() {
                continue;
            }
            let note = trimmed.to_string();
            if !self.context_overlay.notes.contains(&note) {
                self.context_overlay.notes.push(note);
            }
        }
    }

    pub(crate) fn remove_context_notes(&mut self, note_indices: &[usize]) {
        let mut indices = note_indices.to_vec();
        indices.sort_unstable();
        indices.dedup();
        for idx in indices.into_iter().rev() {
            if idx < self.context_overlay.notes.len() {
                self.context_overlay.notes.remove(idx);
            }
        }
    }

    pub(crate) fn clear_context_notes(&mut self) {
        self.context_overlay.notes.clear();
    }

    pub(crate) fn upsert_context_replacements(&mut self, updates: Vec<(u64, String)>) {
        for (rid, text) in updates {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.context_overlay
                .replacements_by_rid
                .insert(rid, trimmed.to_string());
        }
    }

    pub(crate) fn clear_context_replacements_for(&mut self, rids: &[u64]) {
        for rid in rids {
            self.context_overlay.replacements_by_rid.remove(rid);
        }
    }

    pub(crate) fn clear_context_replacements(&mut self) {
        self.context_overlay.replacements_by_rid.clear();
    }

    pub(crate) fn set_include_mask(&mut self, included_indices: Option<BTreeSet<usize>>) {
        let rids = self.history_rids_snapshot();
        let mask = included_indices.map(|included| {
            let mut out = BTreeSet::new();
            for idx in included {
                if let Some(rid) = rids.get(idx).copied() {
                    out.insert(rid);
                }
            }
            out
        });
        self.context_inclusion_mask = mask;
    }

    pub(crate) fn set_context_inclusion(&mut self, indices: &[usize], included: bool) {
        let rids = self.history_rids_snapshot_lenient();
        if included {
            let Some(mask) = self.context_inclusion_mask.as_mut() else {
                return;
            };
            for &idx in indices {
                if let Some(rid) = rids.get(idx).copied() {
                    mask.insert(rid);
                }
            }
            return;
        }

        let mask = self
            .context_inclusion_mask
            .get_or_insert_with(|| rids.iter().copied().collect());
        for &idx in indices {
            if let Some(rid) = rids.get(idx).copied() {
                mask.remove(&rid);
            }
        }
    }

    pub(crate) fn build_context_items_event(&self) -> ContextItemsEvent {
        let (items, rids) = self.history.get_history_for_prompt_with_rids_lenient();

        let mut out: Vec<ContextItemSummary> = Vec::with_capacity(items.len());
        for (index, (item, rid)) in items.into_iter().zip(rids).enumerate() {
            let category = prune_category_for_item(&item);
            let included = self
                .context_inclusion_mask
                .as_ref()
                .is_none_or(|mask| mask.contains(&rid))
                || matches!(
                    category,
                    PruneCategory::EnvironmentContext | PruneCategory::UserInstructions
                );

            out.push(ContextItemSummary {
                index,
                category,
                preview: preview_for_item(&item),
                included,
                id: Some(rid_to_string(rid)),
            });
        }

        ContextItemsEvent { items: out }
    }

    pub(crate) fn prompt_snapshot(&self) -> Vec<ResponseItem> {
        let mut history = self.history.clone();
        let (items, rids) = history.get_history_for_prompt_with_rids();

        let mut out = Vec::new();
        for (item, rid) in items.into_iter().zip(rids) {
            let category = prune_category_for_item(&item);
            let included = self
                .context_inclusion_mask
                .as_ref()
                .is_none_or(|mask| mask.contains(&rid))
                || matches!(
                    category,
                    PruneCategory::EnvironmentContext | PruneCategory::UserInstructions
                );
            if !included {
                continue;
            }

            let replaced = self
                .context_overlay
                .replacements_by_rid
                .get(&rid)
                .map(String::as_str)
                .and_then(|text| apply_replacement(&item, text));
            out.push(replaced.unwrap_or(item));
        }

        crate::context_manager::remove_orphan_outputs_lenient(&mut out);

        if !self.context_overlay.notes.is_empty() {
            let note_item = build_notes_item(&self.context_overlay.notes);
            let insert_at = out
                .iter()
                .position(is_environment_context_item)
                .map(|idx| idx + 1)
                .unwrap_or(0);
            out.insert(insert_at, note_item);
        }

        out
    }

    pub(crate) fn prompt_snapshot_lenient(&self) -> Vec<ResponseItem> {
        let (items, rids) = self.history.get_history_for_prompt_with_rids_lenient();

        let mut out = Vec::new();
        for (item, rid) in items.into_iter().zip(rids) {
            let category = prune_category_for_item(&item);
            let included = self
                .context_inclusion_mask
                .as_ref()
                .is_none_or(|mask| mask.contains(&rid))
                || matches!(
                    category,
                    PruneCategory::EnvironmentContext | PruneCategory::UserInstructions
                );
            if !included {
                continue;
            }

            let replaced = self
                .context_overlay
                .replacements_by_rid
                .get(&rid)
                .map(String::as_str)
                .and_then(|text| apply_replacement(&item, text));
            out.push(replaced.unwrap_or(item));
        }

        crate::context_manager::remove_orphan_outputs_lenient(&mut out);

        if !self.context_overlay.notes.is_empty() {
            let note_item = build_notes_item(&self.context_overlay.notes);
            let insert_at = out
                .iter()
                .position(is_environment_context_item)
                .map(|idx| idx + 1)
                .unwrap_or(0);
            out.insert(insert_at, note_item);
        }

        out
    }

    pub(crate) fn prune_by_indices(&mut self, indices: Vec<usize>) -> PruneByIndicesResult {
        let mut history = self.history.clone();
        let (items, rids) = history.get_history_for_prompt_with_rids();
        prune_by_indices_with_snapshot(self, indices, items, rids)
    }

    pub(crate) fn prune_by_indices_lenient(&mut self, indices: Vec<usize>) -> PruneByIndicesResult {
        let (items, rids) = self.history.get_history_for_prompt_with_rids_lenient();
        prune_by_indices_with_snapshot(self, indices, items, rids)
    }
}

fn prune_by_indices_with_snapshot(
    state: &mut SessionState,
    indices: Vec<usize>,
    items: Vec<ResponseItem>,
    rids: Vec<u64>,
) -> PruneByIndicesResult {
    let mut rids_to_delete: BTreeSet<u64> = BTreeSet::new();
    let mut deleted_indices: Vec<usize> = Vec::new();

    for idx in indices {
        if let Some(rid) = rids.get(idx).copied() {
            rids_to_delete.insert(rid);
            deleted_indices.push(idx);
        }
    }

    // Cascade deletions for call/output pairs (tool_outputs cascade).
    for rid in rids_to_delete.clone() {
        let Some(pos) = rids.iter().position(|r| *r == rid) else {
            continue;
        };
        let Some(call_id) = call_id_for_item(items.get(pos)) else {
            continue;
        };
        for (other_item, other_rid) in items.iter().zip(rids.iter().copied()) {
            if call_id_for_item(Some(other_item)).is_some_and(|cid| cid == call_id) {
                rids_to_delete.insert(other_rid);
            }
        }
    }

    if rids_to_delete.is_empty() {
        return PruneByIndicesResult {
            deleted_indices: Vec::new(),
            deleted_rids: Vec::new(),
        };
    }

    // Replace the in-memory history with the pruned prompt-history view.
    let mut new_items = Vec::new();
    let mut new_rids = Vec::new();
    for (item, rid) in items.into_iter().zip(rids.into_iter()) {
        if rids_to_delete.contains(&rid) {
            continue;
        }
        new_items.push(item);
        new_rids.push(rid);
    }
    state.history.replace_with_rids(new_items, new_rids);

    if let Some(mask) = state.context_inclusion_mask.as_mut() {
        for rid in &rids_to_delete {
            mask.remove(rid);
        }
    }
    for rid in &rids_to_delete {
        state.context_overlay.replacements_by_rid.remove(rid);
    }

    PruneByIndicesResult {
        deleted_indices,
        deleted_rids: rids_to_delete.into_iter().collect(),
    }
}

pub(crate) struct PruneByIndicesResult {
    pub(crate) deleted_indices: Vec<usize>,
    pub(crate) deleted_rids: Vec<u64>,
}

fn prune_category_for_item(item: &ResponseItem) -> PruneCategory {
    match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            if UserInstructions::is_user_instructions(content)
                || SkillInstructions::is_skill_instructions(content)
            {
                PruneCategory::UserInstructions
            } else if is_environment_context_message(content) {
                PruneCategory::EnvironmentContext
            } else {
                PruneCategory::UserMessage
            }
        }
        ResponseItem::Message { role, .. } if role == "developer" => {
            PruneCategory::UserInstructions
        }
        ResponseItem::Message { .. } => PruneCategory::AssistantMessage,
        ResponseItem::Reasoning { .. } => PruneCategory::Reasoning,
        ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::WebSearchCall { .. } => PruneCategory::ToolCall,
        ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. } => {
            PruneCategory::ToolOutput
        }
        // Not expected in prompt history; treat as tool call noise.
        ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Other
        | ResponseItem::Compaction { .. } => PruneCategory::ToolCall,
    }
}

fn preview_for_item(item: &ResponseItem) -> String {
    const MAX: usize = 80;

    let raw = match item {
        ResponseItem::Message { role, content, .. } => {
            let text = first_text(content).unwrap_or("");
            format!("{role}: {text}")
        }
        ResponseItem::FunctionCall { name, .. } => format!("tool call: {name}"),
        ResponseItem::CustomToolCall { name, .. } => format!("tool call: {name}"),
        ResponseItem::LocalShellCall { .. } => "tool call: local_shell".to_string(),
        ResponseItem::WebSearchCall { .. } => "tool call: web_search".to_string(),
        ResponseItem::FunctionCallOutput { output, .. } => {
            tool_output_preview_line(&output.content).to_string()
        }
        ResponseItem::CustomToolCallOutput { output, .. } => {
            tool_output_preview_line(output).to_string()
        }
        ResponseItem::Reasoning { summary, .. } => summary
            .first()
            .map(|s| match s {
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                    text.as_str()
                }
            })
            .unwrap_or("reasoning")
            .to_string(),
        ResponseItem::GhostSnapshot { .. } => "ghost snapshot".to_string(),
        ResponseItem::Compaction { encrypted_content } => {
            format!("compaction ({})", encrypted_content.len())
        }
        ResponseItem::Other => String::new(),
    };

    let trimmed = raw.trim();
    let first_line = trimmed.split('\n').next().unwrap_or("");
    if first_line.len() <= MAX {
        first_line.to_string()
    } else {
        let slice = codex_utils_string::take_bytes_at_char_boundary(first_line, MAX);
        if slice.len() < first_line.len() {
            format!("{slice}…")
        } else {
            slice.to_string()
        }
    }
}

fn tool_output_preview_line(text: &str) -> &str {
    let trimmed = text.trim();
    let mut fallback = None;
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        fallback.get_or_insert(line);
        if is_tool_output_boilerplate_line(line) {
            continue;
        }
        return line;
    }

    fallback.unwrap_or("")
}

fn is_tool_output_boilerplate_line(line: &str) -> bool {
    line == "Output:"
        || line.starts_with("Chunk ID:")
        || line.starts_with("Context left:")
        || line.starts_with("Exit code:")
        || line.starts_with("Wall time:")
        || line.starts_with("Original token count:")
        || line.starts_with("Total output lines:")
        || line.starts_with("Process exited with code")
        || line.starts_with("Process running with session ID")
}

fn is_environment_context_message(content: &[ContentItem]) -> bool {
    let Some(text) = first_text(content) else {
        return false;
    };
    starts_with_case_insensitive(text.trim(), ENVIRONMENT_CONTEXT_OPEN_TAG)
}

fn is_environment_context_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            is_environment_context_message(content)
        }
        _ => false,
    }
}

fn first_text(content: &[ContentItem]) -> Option<&str> {
    content.iter().find_map(|item| match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => Some(text.as_str()),
        ContentItem::InputImage { .. } => None,
    })
}

fn starts_with_case_insensitive(text: &str, prefix: &str) -> bool {
    let pl = prefix.len();
    match text.get(..pl) {
        Some(head) => head.eq_ignore_ascii_case(prefix),
        None => false,
    }
}

fn call_id_for_item(item: Option<&ResponseItem>) -> Option<&str> {
    match item? {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

fn call_id_for_call_item(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

fn call_id_for_output_item(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        _ => None,
    }
}

fn call_id_for_tool_item(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

fn apply_replacement(item: &ResponseItem, replacement: &str) -> Option<ResponseItem> {
    let trimmed = replacement.trim();
    if trimmed.is_empty() {
        return None;
    }

    match item {
        ResponseItem::FunctionCallOutput { call_id, output } => {
            Some(ResponseItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: codex_protocol::models::FunctionCallOutputPayload {
                    content: trimmed.to_string(),
                    content_items: None,
                    success: output.success,
                },
            })
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => {
            Some(ResponseItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: trimmed.to_string(),
            })
        }
        ResponseItem::Reasoning { .. } => Some(ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: format!(
                    "{REASONING_CONTEXT_OPEN_TAG}\n{trimmed}\n{REASONING_CONTEXT_CLOSE_TAG}"
                ),
            }],
        }),
        _ => None,
    }
}

fn build_notes_item(notes: &[String]) -> ResponseItem {
    let mut reasoning_blocks: Vec<&str> = Vec::new();
    let mut tool_blocks: Vec<&str> = Vec::new();
    let mut other_notes: Vec<&str> = Vec::new();

    for note in notes {
        let trimmed = note.trim();
        if trimmed.is_empty() {
            continue;
        }
        if starts_with_case_insensitive(trimmed, TOOL_CONTEXT_OPEN_TAG) {
            tool_blocks.push(trimmed);
        } else if starts_with_case_insensitive(trimmed, REASONING_CONTEXT_OPEN_TAG) {
            reasoning_blocks.push(trimmed);
        } else {
            other_notes.push(trimmed);
        }
    }

    let mut text = String::new();
    for blocks in [tool_blocks, reasoning_blocks] {
        for block in blocks {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(block);
        }
    }

    if !other_notes.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("Pinned notes:");
        for note in other_notes {
            text.push_str("\n- ");
            text.push_str(note);
        }
    }
    if text.trim().is_empty() {
        text = "Pinned notes:".to_string();
    }
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText { text }],
    }
}

// Sometimes new snapshots don't include credits or plan information.
fn merge_rate_limit_fields(
    previous: Option<&RateLimitSnapshot>,
    mut snapshot: RateLimitSnapshot,
) -> RateLimitSnapshot {
    if snapshot.credits.is_none() {
        snapshot.credits = previous.and_then(|prior| prior.credits.clone());
    }
    if snapshot.plan_type.is_none() {
        snapshot.plan_type = previous.and_then(|prior| prior.plan_type);
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn tool_output_preview_skips_boilerplate() {
        let text = "\nContext left: 42%\nChunk ID: abc\nWall time: 0.1 seconds\nOutput:\nhello\n";
        assert_eq!(tool_output_preview_line(text), "hello");
    }

    #[test]
    fn tool_output_preview_skips_original_token_count() {
        let text = "Original token count: 123\nOutput:\nreal output line\n";
        assert_eq!(tool_output_preview_line(text), "real output line");
    }

    #[test]
    fn tool_output_preview_falls_back_to_first_non_empty_line() {
        let text = "\n\n   \nhello\nChunk ID: abc\n";
        assert_eq!(tool_output_preview_line(text), "hello");
    }
}
