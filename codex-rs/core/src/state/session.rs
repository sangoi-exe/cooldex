//! Session-wide mutable state.

use codex_protocol::models::ResponseItem;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use tokio::task::JoinHandle;

use crate::codex::PreviousTurnSettings;
use crate::codex::SessionConfiguration;
use crate::context_manager::ContextManager;
use crate::contextual_user_message::AGENTS_MD_FRAGMENT;
use crate::contextual_user_message::SKILL_FRAGMENT;
use crate::error::Result as CodexResult;
use crate::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use crate::protocol::PINNED_NOTES_CLOSE_TAG;
use crate::protocol::PINNED_NOTES_OPEN_TAG;
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
use crate::tasks::RegularTask;
use crate::truncate::TruncationPolicy;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::openai_models::InputModality;
use codex_protocol::protocol::TurnContextItem;

/// Persistent, session-scoped state previously stored directly on `Session`.
pub(crate) struct SessionState {
    pub(crate) session_configuration: SessionConfiguration,
    pub(crate) history: ContextManager,
    context_inclusion_mask: Option<BTreeSet<u64>>,
    context_overlay: ContextOverlay,
    history_rids: Vec<u64>,
    next_history_rid: u64,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) dependency_env: HashMap<String, String>,
    pub(crate) mcp_dependency_prompted: HashSet<String>,
    /// Settings used by the latest regular user turn, used for turn-to-turn
    /// model/realtime handling on subsequent regular turns (including full-context
    /// reinjection after resume or `/compact`).
    previous_turn_settings: Option<PreviousTurnSettings>,
    /// Startup regular task pre-created during session initialization.
    pub(crate) startup_regular_task: Option<JoinHandle<CodexResult<RegularTask>>>,
    pub(crate) active_mcp_tool_selection: Option<Vec<String>>,
    pub(crate) active_connector_selection: HashSet<String>,
}

#[derive(Clone)]
pub(crate) struct SessionStateCheckpoint {
    history: ContextManager,
    context_inclusion_mask: Option<BTreeSet<u64>>,
    context_overlay: ContextOverlay,
    history_rids: Vec<u64>,
    next_history_rid: u64,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new(session_configuration: SessionConfiguration) -> Self {
        Self {
            session_configuration,
            history: ContextManager::new(),
            context_inclusion_mask: None,
            context_overlay: ContextOverlay::default(),
            history_rids: Vec::new(),
            next_history_rid: 0,
            latest_rate_limits: None,
            server_reasoning_included: false,
            dependency_env: HashMap::new(),
            mcp_dependency_prompted: HashSet::new(),
            previous_turn_settings: None,
            startup_regular_task: None,
            active_mcp_tool_selection: None,
            active_connector_selection: HashSet::new(),
        }
    }

    // History helpers
    pub(crate) fn record_items<I>(&mut self, items: I, policy: TruncationPolicy)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        self.assert_history_alignment();

        let before_len = self.history.raw_items().len();
        self.history.record_items(items, policy);
        let after_len = self.history.raw_items().len();

        assert!(
            after_len >= before_len,
            "history length shrank during record_items: before={before_len} after={after_len}"
        );

        let appended_count = after_len.saturating_sub(before_len);
        if appended_count == 0 {
            return;
        }

        let new_rids = self.allocate_rids(appended_count);
        if let Some(mask) = self.context_inclusion_mask.as_mut() {
            for rid in &new_rids {
                mask.insert(*rid);
            }
        }
        self.history_rids.extend(new_rids);
    }

    pub(crate) fn previous_turn_settings(&self) -> Option<PreviousTurnSettings> {
        self.previous_turn_settings.clone()
    }
    pub(crate) fn set_previous_turn_settings(
        &mut self,
        previous_turn_settings: Option<PreviousTurnSettings>,
    ) {
        self.previous_turn_settings = previous_turn_settings;
    }

    pub(crate) fn clone_history(&self) -> ContextManager {
        self.history.clone()
    }

    pub(crate) fn replace_history(
        &mut self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) {
        self.history.replace(items);
        self.history
            .set_reference_context_item(reference_context_item);
        self.reassign_rids_for_current_history();
        self.context_inclusion_mask = None;
        self.context_overlay = ContextOverlay::default();
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.history.set_token_info(info);
    }

    pub(crate) fn set_reference_context_item(&mut self, item: Option<TurnContextItem>) {
        self.history.set_reference_context_item(item);
    }

    pub(crate) fn reference_context_item(&self) -> Option<TurnContextItem> {
        self.history.reference_context_item()
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

    // manage_context state APIs
    pub(crate) fn history_snapshot_lenient(&self) -> Vec<ResponseItem> {
        self.assert_history_alignment();
        self.history.raw_items().to_vec()
    }

    pub(crate) fn history_rids_snapshot(&self) -> Vec<u64> {
        self.assert_history_alignment();
        self.history_rids.clone()
    }

    pub(crate) fn history_rids_snapshot_lenient(&self) -> Vec<u64> {
        self.history_rids_snapshot()
    }

    pub(crate) fn history_snapshot_with_rids_lenient(&self) -> (Vec<ResponseItem>, Vec<u64>) {
        (
            self.history_snapshot_lenient(),
            self.history_rids_snapshot_lenient(),
        )
    }

    pub(crate) fn context_overlay_snapshot(&self) -> ContextOverlay {
        self.context_overlay.clone()
    }

    pub(crate) fn manage_context_checkpoint(&self) -> SessionStateCheckpoint {
        self.assert_history_alignment();
        SessionStateCheckpoint {
            history: self.history.clone(),
            context_inclusion_mask: self.context_inclusion_mask.clone(),
            context_overlay: self.context_overlay.clone(),
            history_rids: self.history_rids.clone(),
            next_history_rid: self.next_history_rid,
        }
    }

    pub(crate) fn restore_manage_context_checkpoint(&mut self, checkpoint: SessionStateCheckpoint) {
        self.history = checkpoint.history;
        self.context_inclusion_mask = checkpoint.context_inclusion_mask;
        self.context_overlay = checkpoint.context_overlay;
        self.history_rids = checkpoint.history_rids;
        self.next_history_rid = checkpoint.next_history_rid;
        self.assert_history_alignment();
    }

    pub(crate) fn add_context_notes(&mut self, notes: Vec<String>) {
        for note in notes {
            let trimmed = note.trim();
            if trimmed.is_empty() {
                continue;
            }
            let note_string = trimmed.to_string();
            if !self.context_overlay.notes.contains(&note_string) {
                self.context_overlay.notes.push(note_string);
            }
        }
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

    pub(crate) fn set_context_inclusion(&mut self, indices: &[usize], included: bool) {
        let rids = self.history_rids_snapshot_lenient();
        if included {
            let Some(mask) = self.context_inclusion_mask.as_mut() else {
                return;
            };
            for index in indices {
                if let Some(rid) = rids.get(*index).copied() {
                    mask.insert(rid);
                }
            }
            return;
        }

        let mask = self
            .context_inclusion_mask
            .get_or_insert_with(|| rids.iter().copied().collect());
        for index in indices {
            if let Some(rid) = rids.get(*index).copied() {
                mask.remove(&rid);
            }
        }
    }

    pub(crate) fn build_context_items_event(&self) -> ContextItemsEvent {
        let (items, rids) = self.history_snapshot_with_rids_lenient();

        let mut output_items: Vec<ContextItemSummary> = Vec::with_capacity(items.len());
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

            output_items.push(ContextItemSummary {
                index,
                category,
                preview: preview_for_item(&item),
                included,
                id: Some(rid_to_string(rid)),
            });
        }

        ContextItemsEvent {
            items: output_items,
        }
    }

    pub(crate) fn prompt_snapshot_lenient(&self) -> Vec<ResponseItem> {
        let (items, rids) = self.history_snapshot_with_rids_lenient();
        let mut output_items: Vec<ResponseItem> = Vec::new();

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
            output_items.push(replaced.unwrap_or(item));
        }

        crate::context_manager::remove_orphan_outputs_lenient(&mut output_items);
        if self.context_inclusion_mask.is_some()
            || !self.context_overlay.replacements_by_rid.is_empty()
        {
            crate::context_manager::ensure_call_outputs_present_lenient(&mut output_items);
        }

        if !self.context_overlay.notes.is_empty() {
            let note_items = build_note_items(&self.context_overlay.notes);
            let insert_at = output_items
                .iter()
                .position(is_environment_context_item)
                .map(|index| index + 1)
                .unwrap_or(0);
            output_items.splice(insert_at..insert_at, note_items);
        }

        output_items
    }

    pub(crate) fn prompt_snapshot_for_model(
        &self,
        input_modalities: &[InputModality],
    ) -> Vec<ResponseItem> {
        if self.context_inclusion_mask.is_none()
            && self.context_overlay.replacements_by_rid.is_empty()
            && self.context_overlay.notes.is_empty()
        {
            return self.clone_history().for_prompt(input_modalities);
        }

        let mut manager = ContextManager::new();
        manager.replace(self.prompt_snapshot_lenient());
        manager.for_prompt(input_modalities)
    }

    pub(crate) fn set_startup_regular_task(&mut self, task: JoinHandle<CodexResult<RegularTask>>) {
        self.startup_regular_task = Some(task);
    }

    pub(crate) fn take_startup_regular_task(
        &mut self,
    ) -> Option<JoinHandle<CodexResult<RegularTask>>> {
        self.startup_regular_task.take()
    }

    pub(crate) fn merge_mcp_tool_selection(&mut self, tool_names: Vec<String>) -> Vec<String> {
        if tool_names.is_empty() {
            return self.active_mcp_tool_selection.clone().unwrap_or_default();
        }

        let mut merged = self.active_mcp_tool_selection.take().unwrap_or_default();
        let mut seen: HashSet<String> = merged.iter().cloned().collect();

        for tool_name in tool_names {
            if seen.insert(tool_name.clone()) {
                merged.push(tool_name);
            }
        }

        self.active_mcp_tool_selection = Some(merged.clone());
        merged
    }

    pub(crate) fn set_mcp_tool_selection(&mut self, tool_names: Vec<String>) {
        if tool_names.is_empty() {
            self.active_mcp_tool_selection = None;
            return;
        }

        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for tool_name in tool_names {
            if seen.insert(tool_name.clone()) {
                selected.push(tool_name);
            }
        }

        self.active_mcp_tool_selection = if selected.is_empty() {
            None
        } else {
            Some(selected)
        };
    }

    pub(crate) fn get_mcp_tool_selection(&self) -> Option<Vec<String>> {
        self.active_mcp_tool_selection.clone()
    }

    pub(crate) fn clear_mcp_tool_selection(&mut self) {
        self.active_mcp_tool_selection = None;
    }

    // Adds connector IDs to the active set and returns the merged selection.
    pub(crate) fn merge_connector_selection<I>(&mut self, connector_ids: I) -> HashSet<String>
    where
        I: IntoIterator<Item = String>,
    {
        self.active_connector_selection.extend(connector_ids);
        self.active_connector_selection.clone()
    }

    // Returns the current connector selection tracked on session state.
    pub(crate) fn get_connector_selection(&self) -> HashSet<String> {
        self.active_connector_selection.clone()
    }

    // Removes all currently tracked connector selections.
    pub(crate) fn clear_connector_selection(&mut self) {
        self.active_connector_selection.clear();
    }

    fn assert_history_alignment(&self) {
        let history_len = self.history.raw_items().len();
        let rid_len = self.history_rids.len();
        assert_eq!(
            history_len, rid_len,
            "SessionState history/rid invariant broken: history_len={history_len} rid_len={rid_len}"
        );
    }

    fn allocate_rids(&mut self, count: usize) -> Vec<u64> {
        let mut output = Vec::with_capacity(count);
        for _ in 0..count {
            let rid = self.next_history_rid;
            self.next_history_rid = self.next_history_rid.saturating_add(1);
            output.push(rid);
        }
        output
    }

    fn reassign_rids_for_current_history(&mut self) {
        let history_len = self.history.raw_items().len();
        let new_rids = self.allocate_rids(history_len);
        self.history_rids = new_rids;
    }
}

fn prune_category_for_item(item: &ResponseItem) -> PruneCategory {
    match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            if first_text(content).is_some_and(|text| {
                AGENTS_MD_FRAGMENT.matches_text(text) || SKILL_FRAGMENT.matches_text(text)
            }) {
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
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. } => PruneCategory::ToolCall,
        ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. } => {
            PruneCategory::ToolOutput
        }
        ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Other
        | ResponseItem::Compaction { .. } => PruneCategory::ToolCall,
    }
}

fn preview_for_item(item: &ResponseItem) -> String {
    const MAX_BYTES: usize = 80;

    let raw = match item {
        ResponseItem::Message { role, content, .. } => {
            let text = first_text(content).unwrap_or("");
            format!("{role}: {text}")
        }
        ResponseItem::FunctionCall { name, .. } => format!("tool call: {name}"),
        ResponseItem::CustomToolCall { name, .. } => format!("tool call: {name}"),
        ResponseItem::LocalShellCall { .. } => "tool call: local_shell".to_string(),
        ResponseItem::WebSearchCall { .. } => "tool call: web_search".to_string(),
        ResponseItem::ImageGenerationCall { .. } => "tool call: image_generation".to_string(),
        ResponseItem::FunctionCallOutput { output, .. } => {
            let text = output.body.to_text().unwrap_or_default();
            tool_output_preview_line(&text).to_string()
        }
        ResponseItem::CustomToolCallOutput { output, .. } => {
            let text = output.body.to_text().unwrap_or_default();
            tool_output_preview_line(&text).to_string()
        }
        ResponseItem::Reasoning { summary, .. } => summary
            .first()
            .map(|summary_item| match summary_item {
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
    if first_line.len() <= MAX_BYTES {
        first_line.to_string()
    } else {
        let slice = codex_utils_string::take_bytes_at_char_boundary(first_line, MAX_BYTES);
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
        let candidate = line.trim();
        if candidate.is_empty() {
            continue;
        }
        fallback.get_or_insert(candidate);
        if is_tool_output_boilerplate_line(candidate) {
            continue;
        }
        return candidate;
    }

    fallback.unwrap_or("")
}

fn is_tool_output_boilerplate_line(line: &str) -> bool {
    line == "Output:"
        || line.starts_with("Chunk ID:")
        || line.starts_with("call_id:")
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
    match text.get(..prefix.len()) {
        Some(head) => head.eq_ignore_ascii_case(prefix),
        None => false,
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
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(trimmed.to_string()),
                    success: output.success,
                },
            })
        }
        ResponseItem::CustomToolCallOutput { call_id, output } => {
            Some(ResponseItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(trimmed.to_string()),
                    success: output.success,
                },
            })
        }
        ResponseItem::Reasoning { .. } => Some(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!(
                    "{REASONING_CONTEXT_OPEN_TAG}\n{trimmed}\n{REASONING_CONTEXT_CLOSE_TAG}"
                ),
            }],
            end_turn: None,
            phase: None,
        }),
        _ => None,
    }
}

fn build_note_items(notes: &[String]) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    let mut other_notes: Vec<&str> = Vec::new();

    for note in notes {
        let trimmed = note.trim();
        if trimmed.is_empty() {
            continue;
        }
        if starts_with_case_insensitive(trimmed, TOOL_CONTEXT_OPEN_TAG)
            || starts_with_case_insensitive(trimmed, REASONING_CONTEXT_OPEN_TAG)
        {
            items.push(ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: trimmed.to_string(),
                }],
                end_turn: None,
                phase: None,
            });
        } else {
            other_notes.push(trimmed);
        }
    }

    if !other_notes.is_empty() {
        let mut notes_text = String::from("Pinned notes:");
        for note in other_notes {
            notes_text.push_str("\n- ");
            notes_text.push_str(note);
        }
        let text = format!("{PINNED_NOTES_OPEN_TAG}\n{notes_text}\n{PINNED_NOTES_CLOSE_TAG}");
        items.push(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text }],
            end_turn: None,
            phase: None,
        });
    }

    items
}

// Sometimes new snapshots don't include credits or plan information.
// Preserve those from the previous snapshot when missing. For `limit_id`, treat
// missing values as the default `"codex"` bucket.
fn merge_rate_limit_fields(
    previous: Option<&RateLimitSnapshot>,
    mut snapshot: RateLimitSnapshot,
) -> RateLimitSnapshot {
    if snapshot.limit_id.is_none() {
        snapshot.limit_id = Some("codex".to_string());
    }
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
    use crate::codex::make_session_configuration_for_tests;
    use crate::protocol::RateLimitWindow;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn merge_mcp_tool_selection_deduplicates_and_preserves_order() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);

        let merged = state.merge_mcp_tool_selection(vec![
            "mcp__rmcp__echo".to_string(),
            "mcp__rmcp__image".to_string(),
            "mcp__rmcp__echo".to_string(),
        ]);
        assert_eq!(
            merged,
            vec![
                "mcp__rmcp__echo".to_string(),
                "mcp__rmcp__image".to_string(),
            ]
        );

        let merged = state.merge_mcp_tool_selection(vec![
            "mcp__rmcp__image".to_string(),
            "mcp__rmcp__search".to_string(),
        ]);
        assert_eq!(
            merged,
            vec![
                "mcp__rmcp__echo".to_string(),
                "mcp__rmcp__image".to_string(),
                "mcp__rmcp__search".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn merge_mcp_tool_selection_empty_input_is_noop() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        state.merge_mcp_tool_selection(vec![
            "mcp__rmcp__echo".to_string(),
            "mcp__rmcp__image".to_string(),
        ]);

        let merged = state.merge_mcp_tool_selection(Vec::new());
        assert_eq!(
            merged,
            vec![
                "mcp__rmcp__echo".to_string(),
                "mcp__rmcp__image".to_string(),
            ]
        );
        assert_eq!(
            state.get_mcp_tool_selection(),
            Some(vec![
                "mcp__rmcp__echo".to_string(),
                "mcp__rmcp__image".to_string(),
            ])
        );
    }

    #[tokio::test]
    async fn clear_mcp_tool_selection_removes_selection() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        state.merge_mcp_tool_selection(vec!["mcp__rmcp__echo".to_string()]);

        state.clear_mcp_tool_selection();

        assert_eq!(state.get_mcp_tool_selection(), None);
    }

    #[tokio::test]
    async fn set_mcp_tool_selection_deduplicates_and_preserves_order() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        state.merge_mcp_tool_selection(vec!["mcp__rmcp__old".to_string()]);

        state.set_mcp_tool_selection(vec![
            "mcp__rmcp__echo".to_string(),
            "mcp__rmcp__image".to_string(),
            "mcp__rmcp__echo".to_string(),
            "mcp__rmcp__search".to_string(),
        ]);

        assert_eq!(
            state.get_mcp_tool_selection(),
            Some(vec![
                "mcp__rmcp__echo".to_string(),
                "mcp__rmcp__image".to_string(),
                "mcp__rmcp__search".to_string(),
            ])
        );
    }

    #[tokio::test]
    async fn set_mcp_tool_selection_empty_input_clears_selection() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        state.merge_mcp_tool_selection(vec!["mcp__rmcp__echo".to_string()]);

        state.set_mcp_tool_selection(Vec::new());

        assert_eq!(state.get_mcp_tool_selection(), None);
    }

    #[tokio::test]
    // Verifies connector merging deduplicates repeated IDs.
    async fn merge_connector_selection_deduplicates_entries() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        let merged = state.merge_connector_selection([
            "calendar".to_string(),
            "calendar".to_string(),
            "drive".to_string(),
        ]);

        assert_eq!(
            merged,
            HashSet::from(["calendar".to_string(), "drive".to_string()])
        );
    }

    #[tokio::test]
    // Verifies clearing connector selection removes all saved IDs.
    async fn clear_connector_selection_removes_entries() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        state.merge_connector_selection(["calendar".to_string()]);

        state.clear_connector_selection();

        assert_eq!(state.get_connector_selection(), HashSet::new());
    }

    #[tokio::test]
    async fn set_rate_limits_defaults_limit_id_to_codex_when_missing() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);

        state.set_rate_limits(RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 12.0,
                window_minutes: Some(60),
                resets_at: Some(100),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        });

        assert_eq!(
            state
                .latest_rate_limits
                .as_ref()
                .and_then(|value| value.limit_id.clone()),
            Some("codex".to_string())
        );
    }

    #[tokio::test]
    async fn set_rate_limits_defaults_to_codex_when_limit_id_missing_after_other_bucket() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);

        state.set_rate_limits(RateLimitSnapshot {
            limit_id: Some("codex_other".to_string()),
            limit_name: Some("codex_other".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: 20.0,
                window_minutes: Some(60),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        });
        state.set_rate_limits(RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(60),
                resets_at: Some(300),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        });

        assert_eq!(
            state
                .latest_rate_limits
                .as_ref()
                .and_then(|value| value.limit_id.clone()),
            Some("codex".to_string())
        );
    }

    #[tokio::test]
    async fn set_rate_limits_carries_credits_and_plan_type_from_codex_to_codex_other() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);

        state.set_rate_limits(RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: 10.0,
                window_minutes: Some(60),
                resets_at: Some(100),
            }),
            secondary: None,
            credits: Some(crate::protocol::CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("50".to_string()),
            }),
            plan_type: Some(codex_protocol::account::PlanType::Plus),
        });

        state.set_rate_limits(RateLimitSnapshot {
            limit_id: Some("codex_other".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(120),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        });

        assert_eq!(
            state.latest_rate_limits,
            Some(RateLimitSnapshot {
                limit_id: Some("codex_other".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    used_percent: 30.0,
                    window_minutes: Some(120),
                    resets_at: Some(200),
                }),
                secondary: None,
                credits: Some(crate::protocol::CreditsSnapshot {
                    has_credits: true,
                    unlimited: false,
                    balance: Some("50".to_string()),
                }),
                plan_type: Some(codex_protocol::account::PlanType::Plus),
            })
        );
    }

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

    #[test]
    fn apply_replacement_for_reasoning_uses_user_input_context_block() {
        let reasoning = ResponseItem::Reasoning {
            id: "rid-1".to_string(),
            summary: vec![
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText {
                    text: "summary".to_string(),
                },
            ],
            content: None,
            encrypted_content: None,
        };

        let replaced = apply_replacement(&reasoning, "trimmed summary").expect("replacement");
        let ResponseItem::Message { role, content, .. } = replaced else {
            panic!("expected message replacement");
        };
        assert_eq!(role, "user");
        assert!(matches!(
            content.first(),
            Some(ContentItem::InputText { text })
                if text.starts_with(REASONING_CONTEXT_OPEN_TAG)
                    && text.contains("trimmed summary")
                    && text.ends_with(REASONING_CONTEXT_CLOSE_TAG)
        ));
    }

    #[test]
    fn apply_replacement_for_custom_tool_output_preserves_success() {
        let item = ResponseItem::CustomToolCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("old".to_string()),
                success: Some(false),
            },
        };

        let replaced = apply_replacement(&item, "  replacement text  ").expect("replacement");

        assert_eq!(
            replaced,
            ResponseItem::CustomToolCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("replacement text".to_string()),
                    success: Some(false),
                },
            }
        );
    }

    #[tokio::test]
    async fn prompt_snapshot_lenient_renders_context_notes_as_user_messages() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = SessionState::new(session_configuration);
        state.add_context_notes(vec![
            format!(
                "{REASONING_CONTEXT_OPEN_TAG}\nreasoning summary\n{REASONING_CONTEXT_CLOSE_TAG}"
            ),
            format!("{TOOL_CONTEXT_OPEN_TAG}\ntool summary\n</tool_context>"),
            "keep this note".to_string(),
        ]);

        let prompt_items = state.prompt_snapshot_lenient();

        assert!(prompt_items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::Message { role, content, .. }
                    if role == "user"
                        && matches!(
                            content.first(),
                            Some(ContentItem::InputText { text })
                                if text.starts_with(REASONING_CONTEXT_OPEN_TAG)
                        )
            )
        }));
        assert!(prompt_items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::Message { role, content, .. }
                    if role == "user"
                        && matches!(
                            content.first(),
                            Some(ContentItem::InputText { text })
                                if text.starts_with(TOOL_CONTEXT_OPEN_TAG)
                        )
            )
        }));
        assert!(prompt_items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::Message { role, content, .. }
                    if role == "user"
                        && matches!(
                            content.first(),
                            Some(ContentItem::InputText { text })
                                if text.starts_with(PINNED_NOTES_OPEN_TAG)
                                    && text.contains("Pinned notes:")
                                    && text.contains("keep this note")
                                    && text.ends_with(PINNED_NOTES_CLOSE_TAG)
                        )
            )
        }));
    }
}
