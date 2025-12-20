//! Session-wide mutable state.

use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ContextItemSummary;
use codex_protocol::protocol::ContextItemsEvent;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use codex_protocol::protocol::PruneCategory;
use codex_protocol::protocol::PruneRange;
use codex_protocol::protocol::USER_INSTRUCTIONS_OPEN_TAG;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;

use crate::conversation_history::ConversationHistory;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::rid::rid_to_string;

const CONTEXT_NOTES_OPEN_TAG: &str = "<context_notes>";
const CONTEXT_NOTES_CLOSE_TAG: &str = "</context_notes>";

#[derive(Clone, Debug, Default)]
pub(crate) struct ContextOverlay {
    pub(crate) replacements_by_rid: BTreeMap<u64, String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PruneByIndicesResult {
    pub(crate) deleted_indices: Vec<usize>,
    pub(crate) deleted_rids: Vec<u64>,
}

/// Persistent, session-scoped state previously stored directly on `Session`.
#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) history: ConversationHistory,
    history_rids: Vec<u64>,
    next_rid: u64,
    pub(crate) token_info: Option<TokenUsageInfo>,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
    // Optional inclusion mask for Advanced Prune. When None, all items are included.
    include_mask: Option<BTreeSet<usize>>,
    context_overlay: ContextOverlay,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new() -> Self {
        Self {
            history: ConversationHistory::new(),
            ..Default::default()
        }
    }

    // History helpers
    pub(crate) fn record_items<I>(&mut self, items: I)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        let old_len = self.history.len();
        self.history.record_items(items);
        let new_len = self.history.len();
        if let Some(mask) = &mut self.include_mask {
            for idx in old_len..new_len {
                mask.insert(idx);
            }
        }
        if new_len > old_len {
            for _ in old_len..new_len {
                let rid = self.alloc_rid();
                self.history_rids.push(rid);
            }
        }
    }

    pub(crate) fn history_snapshot(&self) -> Vec<ResponseItem> {
        self.history.contents()
    }

    pub(crate) fn history_rids_snapshot(&self) -> Vec<u64> {
        self.history_rids.clone()
    }

    #[cfg(test)]
    pub(crate) fn include_mask_snapshot(&self) -> Option<BTreeSet<usize>> {
        self.include_mask.clone()
    }

    pub(crate) fn replace_history(&mut self, items: Vec<ResponseItem>) {
        let mut rids = Vec::with_capacity(items.len());
        for _ in 0..items.len() {
            rids.push(self.alloc_rid());
        }
        self.replace_history_with_rids(items, rids);
    }

    pub(crate) fn replace_history_with_rids(&mut self, items: Vec<ResponseItem>, rids: Vec<u64>) {
        debug_assert_eq!(items.len(), rids.len());
        self.history.replace(items);
        self.history_rids = rids;
        self.realign_mask_after_replace();
        // Ensure next_rid never goes backwards.
        if let Some(max_rid) = self.history_rids.iter().copied().max() {
            self.next_rid = self.next_rid.max(max_rid.saturating_add(1));
        }
    }

    fn realign_mask_after_replace(&mut self) {
        // Reset include_mask because indices changed completely.
        self.include_mask = None;
    }

    pub(crate) fn set_include_mask(&mut self, mask: Option<BTreeSet<usize>>) {
        self.include_mask = mask;
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<u64>,
    ) {
        self.token_info = TokenUsageInfo::new_or_append(
            &self.token_info,
            &Some(usage.clone()),
            model_context_window,
        );
    }

    pub(crate) fn set_rate_limits(&mut self, snapshot: RateLimitSnapshot) {
        self.latest_rate_limits = Some(snapshot);
    }

    pub(crate) fn token_info_and_rate_limits(
        &self,
    ) -> (Option<TokenUsageInfo>, Option<RateLimitSnapshot>) {
        (self.token_info.clone(), self.latest_rate_limits.clone())
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: u64) {
        match &mut self.token_info {
            Some(info) => info.fill_to_context_window(context_window),
            None => {
                self.token_info = Some(TokenUsageInfo::full_context_window(context_window));
            }
        }
    }

    pub(crate) fn context_overlay_snapshot(&self) -> ContextOverlay {
        self.context_overlay.clone()
    }

    pub(crate) fn set_context_overlay(&mut self, overlay: ContextOverlay) {
        self.context_overlay = overlay;
    }

    pub(crate) fn upsert_context_replacements<I>(&mut self, replacements: I)
    where
        I: IntoIterator<Item = (u64, String)>,
    {
        for (rid, text) in replacements {
            self.context_overlay.replacements_by_rid.insert(rid, text);
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

    pub(crate) fn add_context_notes<I>(&mut self, notes: I)
    where
        I: IntoIterator<Item = String>,
    {
        for note in notes {
            let trimmed = note.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.context_overlay.notes.push(trimmed.to_string());
        }
    }

    pub(crate) fn remove_context_notes(&mut self, indices: &[usize]) {
        if indices.is_empty() || self.context_overlay.notes.is_empty() {
            return;
        }

        let mut to_remove: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|idx| *idx < self.context_overlay.notes.len())
            .collect();
        to_remove.sort_unstable();
        to_remove.dedup();
        to_remove.reverse();
        for idx in to_remove {
            self.context_overlay.notes.remove(idx);
        }
    }

    pub(crate) fn clear_context_notes(&mut self) {
        self.context_overlay.notes.clear();
    }

    // Pending input/approval lives in TurnState.
}

impl SessionState {
    /// Return a filtered history after applying the inclusion mask.
    pub(crate) fn filtered_history(&self) -> Vec<ResponseItem> {
        let mut items = self.history_for_prompt();
        normalize_tool_call_pairs(&mut items);
        items
    }

    fn history_for_prompt(&self) -> Vec<ResponseItem> {
        let items = self.history.contents();
        let mask = self.include_mask.as_ref();
        let mut selected: Vec<(Option<u64>, ResponseItem)> = Vec::with_capacity(items.len());

        for (idx, item) in items.into_iter().enumerate() {
            if mask.is_some_and(|m| !m.contains(&idx)) {
                continue;
            }
            let rid = self.history_rids.get(idx).copied();
            selected.push((rid, item));
        }

        let mut out: Vec<ResponseItem> = Vec::with_capacity(selected.len());
        for (rid, mut item) in selected {
            if let Some(rid) = rid
                && let Some(replacement) = self.context_overlay.replacements_by_rid.get(&rid)
            {
                apply_context_replacement(&mut item, replacement);
            }
            out.push(item);
        }

        if !self.context_overlay.notes.is_empty() {
            let insertion_idx = out
                .iter()
                .take_while(|item| {
                    categorize(item).is_some_and(|cat| {
                        matches!(
                            cat,
                            PruneCategory::UserInstructions | PruneCategory::EnvironmentContext
                        )
                    })
                })
                .count();
            out.insert(
                insertion_idx,
                context_notes_message(&self.context_overlay.notes),
            );
        }

        out
    }

    /// Ensure include_mask is initialized to "all included".
    fn ensure_mask_all_included(&mut self) {
        if self.include_mask.is_none() {
            // Use a contents() snapshot to compute length (restored behavior).
            let len = self.history.contents().len();
            self.include_mask = Some((0..len).collect());
        }
    }

    /// Set inclusion for given indices. Ignores out-of-range indices.
    pub(crate) fn set_context_inclusion(&mut self, indices: &[usize], included: bool) {
        self.ensure_mask_all_included();
        if let Some(mask) = &mut self.include_mask {
            // Use a contents() snapshot to compute length (restored behavior).
            let len = self.history.contents().len();
            for &idx in indices {
                if idx >= len {
                    continue;
                }
                if included {
                    mask.insert(idx);
                } else {
                    mask.remove(&idx);
                }
            }
        }
    }

    /// Delete items by index from history and update the inclusion mask accordingly.
    pub(crate) fn prune_by_indices(&mut self, indices: Vec<usize>) -> PruneByIndicesResult {
        let snapshot_items = self.history.contents();
        let snapshot_rids = self.history_rids_snapshot();

        let mut expanded: Vec<usize> = indices
            .into_iter()
            .filter(|idx| *idx < snapshot_items.len())
            .collect();

        // If the user deletes a tool call, also delete its corresponding output(s)
        // so we never leave orphan outputs behind.
        let mut extra_indices: Vec<usize> = Vec::new();
        for &idx in &expanded {
            let Some(item) = snapshot_items.get(idx) else {
                continue;
            };
            match item {
                ResponseItem::FunctionCall { call_id, .. } => {
                    extra_indices.extend(snapshot_items.iter().enumerate().filter_map(|(i, it)| {
                        matches!(it, ResponseItem::FunctionCallOutput { call_id: cid, .. } if cid == call_id)
                            .then_some(i)
                    }));
                }
                ResponseItem::CustomToolCall { call_id, .. } => {
                    extra_indices.extend(snapshot_items.iter().enumerate().filter_map(|(i, it)| {
                        matches!(it, ResponseItem::CustomToolCallOutput { call_id: cid, .. } if cid == call_id)
                            .then_some(i)
                    }));
                }
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => {
                    extra_indices.extend(snapshot_items.iter().enumerate().filter_map(|(i, it)| {
                        matches!(it, ResponseItem::FunctionCallOutput { call_id: cid, .. } if cid == call_id)
                            .then_some(i)
                    }));
                }
                _ => {}
            }
        }
        expanded.extend(extra_indices);

        expanded.sort_unstable();
        expanded.dedup();

        let deleted_rids: Vec<u64> = expanded
            .iter()
            .filter_map(|idx| snapshot_rids.get(*idx).copied())
            .collect();

        let mut items = snapshot_items;
        let mut changed = false;

        let mut descending = expanded.clone();
        descending.reverse();
        for idx in descending {
            if idx < items.len() {
                items.remove(idx);
                if idx < self.history_rids.len() {
                    self.history_rids.remove(idx);
                }
                changed = true;
                if let Some(mask) = &mut self.include_mask {
                    mask.remove(&idx);
                    // Shift indices greater than idx by -1
                    let mut shifted: BTreeSet<usize> = BTreeSet::new();
                    for &m in mask.iter() {
                        shifted.insert(if m > idx { m - 1 } else { m });
                    }
                    *mask = shifted;
                }
            }
        }
        if changed {
            self.history.replace(items);
        }

        PruneByIndicesResult {
            deleted_indices: expanded,
            deleted_rids,
        }
    }

    /// Mark matching categories as excluded (non-destructive prune).
    pub(crate) fn prune_by_categories(
        &mut self,
        categories: &[PruneCategory],
        _range: &PruneRange,
    ) {
        if categories.is_empty() {
            return;
        }
        let items = self.history.contents();
        let mut to_exclude: Vec<usize> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            if let Some(cat) = categorize(item)
                && categories.iter().any(|c| c == &cat)
            {
                to_exclude.push(idx);
            }
        }
        self.set_context_inclusion(&to_exclude, false);
    }

    /// Build a ContextItemsEvent summarizing items and their inclusion state.
    pub(crate) fn build_context_items_event(&self) -> ContextItemsEvent {
        let items = self.history.contents();
        let mask = self.include_mask.as_ref();
        let mut out: Vec<ContextItemSummary> = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            if let Some(category) = categorize(item) {
                let included = match mask {
                    None => true,
                    Some(m) => m.contains(&idx),
                };
                let preview = preview_for(item);
                let id = self.history_rids.get(idx).copied().map(rid_to_string);
                out.push(ContextItemSummary {
                    index: idx,
                    category,
                    preview,
                    included,
                    id,
                });
            }
        }
        ContextItemsEvent {
            total: out.len(),
            items: out,
        }
    }
}

/// Map a ResponseItem to a PruneCategory.
fn categorize(item: &ResponseItem) -> Option<PruneCategory> {
    use ResponseItem::*;
    match item {
        Message { role, content, .. } => {
            if let Some(text) = first_text(content) {
                let t = text.trim();
                if starts_with_case_insensitive(t, ENVIRONMENT_CONTEXT_OPEN_TAG) {
                    return Some(PruneCategory::EnvironmentContext);
                }
                if starts_with_case_insensitive(t, USER_INSTRUCTIONS_OPEN_TAG) {
                    return Some(PruneCategory::UserInstructions);
                }
            }
            if role == "assistant" {
                Some(PruneCategory::AssistantMessage)
            } else if role == "user" {
                Some(PruneCategory::UserMessage)
            } else {
                None
            }
        }
        Reasoning { .. } => Some(PruneCategory::Reasoning),
        FunctionCall { .. }
        | CustomToolCall { .. }
        | LocalShellCall { .. }
        | WebSearchCall { .. } => Some(PruneCategory::ToolCall),
        FunctionCallOutput { .. } | CustomToolCallOutput { .. } => Some(PruneCategory::ToolOutput),
        Other => None,
    }
}

fn first_text(items: &[ContentItem]) -> Option<&str> {
    for c in items {
        match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                return Some(text);
            }
            _ => {}
        }
    }
    None
}

fn starts_with_case_insensitive(text: &str, prefix: &str) -> bool {
    let pl = prefix.len();
    match text.get(..pl) {
        Some(head) => head.eq_ignore_ascii_case(prefix),
        None => false, // not enough bytes or not on a char boundary — cannot match
    }
}

fn preview_for(item: &ResponseItem) -> String {
    use ResponseItem::*;
    const MAX: usize = 80;
    let out = match item {
        Message { role, content, .. } => {
            let raw = first_text(content).unwrap_or("");
            let mut s = raw.trim();
            if let Some(idx) = s.find('\n') {
                s = &s[..idx];
            }
            format!("{role}: {s}")
        }
        Reasoning { .. } => "<reasoning>…".to_string(),
        FunctionCall { name, .. } => format!("tool call: {name}"),
        FunctionCallOutput { output, .. } => {
            let s = output.content.trim();
            format!("tool output: {s}")
        }
        CustomToolCall { name, .. } => format!("tool call: {name}"),
        CustomToolCallOutput { output, .. } => {
            let s = output.trim();
            format!("tool output: {s}")
        }
        LocalShellCall { status, .. } => format!("shell: {status:?}"),
        WebSearchCall { action, .. } => match action {
            codex_protocol::models::WebSearchAction::Search { query } => format!("search: {query}"),
            codex_protocol::models::WebSearchAction::Other => "search".to_string(),
        },
        Other => String::from("other"),
    };
    if out.len() > MAX {
        crate::truncate::truncate_grapheme_head(&out, MAX)
    } else {
        out
    }
}

fn apply_context_replacement(item: &mut ResponseItem, replacement: &str) {
    match item {
        ResponseItem::FunctionCallOutput { output, .. } => {
            output.content = replacement.to_string();
            output.success = None;
        }
        ResponseItem::CustomToolCallOutput { output, .. } => {
            *output = replacement.to_string();
        }
        ResponseItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => {
            *summary = vec![ReasoningItemReasoningSummary::SummaryText {
                text: replacement.to_string(),
            }];
            *content = None;
            *encrypted_content = None;
        }
        _ => {}
    }
}

fn context_notes_message(notes: &[String]) -> ResponseItem {
    let mut lines = Vec::with_capacity(notes.len() + 2);
    lines.push(CONTEXT_NOTES_OPEN_TAG.to_string());
    for note in notes {
        lines.push(format!("- {}", note.trim()));
    }
    lines.push(CONTEXT_NOTES_CLOSE_TAG.to_string());

    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: lines.join("\n"),
        }],
    }
}

fn normalize_tool_call_pairs(items: &mut Vec<ResponseItem>) {
    let mut function_call_ids: HashSet<String> = HashSet::new();
    let mut local_shell_call_ids: HashSet<String> = HashSet::new();
    let mut custom_tool_call_ids: HashSet<String> = HashSet::new();

    for item in items.iter() {
        match item {
            ResponseItem::FunctionCall { call_id, .. } => {
                function_call_ids.insert(call_id.clone());
            }
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                local_shell_call_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                custom_tool_call_ids.insert(call_id.clone());
            }
            _ => {}
        }
    }

    // Drop orphan outputs first (avoids invalid prompt sequences).
    items.retain(|item| match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => {
            function_call_ids.contains(call_id) || local_shell_call_ids.contains(call_id)
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => {
            custom_tool_call_ids.contains(call_id)
        }
        _ => true,
    });

    let function_output_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| match i {
            ResponseItem::FunctionCallOutput { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    let custom_output_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| match i {
            ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();

    // Insert missing outputs right after their calls (small placeholders).
    let mut missing_outputs_to_insert: Vec<(usize, ResponseItem)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                if !function_output_ids.contains(call_id) {
                    missing_outputs_to_insert.push((
                        idx,
                        ResponseItem::FunctionCallOutput {
                            call_id: call_id.clone(),
                            output: FunctionCallOutputPayload {
                                content: "aborted".to_string(),
                                success: Some(false),
                            },
                        },
                    ));
                }
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                if !custom_output_ids.contains(call_id) {
                    missing_outputs_to_insert.push((
                        idx,
                        ResponseItem::CustomToolCallOutput {
                            call_id: call_id.clone(),
                            output: "aborted".to_string(),
                        },
                    ));
                }
            }
            _ => {}
        }
    }

    for (idx, output_item) in missing_outputs_to_insert.into_iter().rev() {
        items.insert(idx + 1, output_item);
    }
}

impl SessionState {
    fn alloc_rid(&mut self) -> u64 {
        let rid = self.next_rid;
        self.next_rid = self.next_rid.saturating_add(1);
        rid
    }
}
