use crate::codex::Session;
use crate::codex::TurnContext;
use crate::context_manager::ContextManager;
use crate::function_tool::FunctionCallError;
use crate::protocol::ContextInclusionItem;
use crate::protocol::ContextOverlayItem;
use crate::protocol::ContextOverlayReplacement;
use crate::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use crate::protocol::RolloutItem;
use crate::rid::parse_rid;
use crate::rid::rid_to_string;
use crate::state::ContextItemsEvent;
use crate::state::ContextOverlay;
use crate::state::PruneCategory;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::user_instructions::SkillInstructions;
use crate::user_instructions::UserInstructions;
use async_trait::async_trait;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;
use serde_json::json;
use sha1::Digest;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

const DEFAULT_MAX_TOP_ITEMS: usize = 10;
const DEFAULT_MAX_ITEMS: usize = 200;

pub struct ManageContextHandler;

#[derive(Debug, Deserialize)]
struct ManageContextToolArgs {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    include_items: Option<bool>,
    #[serde(default)]
    include_notes: Option<bool>,
    #[serde(default)]
    include_token_usage: Option<bool>,
    #[serde(default)]
    include_pairs: Option<bool>,
    #[serde(default)]
    include_internal: Option<bool>,
    #[serde(default)]
    max_items: Option<usize>,
    #[serde(default)]
    max_top_items: Option<usize>,

    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(default)]
    ops: Vec<ManageContextOp>,

    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    include_prompt_preview: Option<bool>,
    #[serde(default)]
    allow_recent: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ManageContextOp {
    op: String,
    #[serde(default)]
    targets: Option<ManageContextTargets>,
    #[serde(default)]
    cascade: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    note_indices: Vec<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct ManageContextTargets {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    indices: Vec<usize>,
    #[serde(default)]
    call_ids: Vec<String>,
}

#[async_trait]
impl ToolHandler for ManageContextHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            payload,
            turn,
            ..
        } = invocation;

        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "manage_context handler received unsupported payload".to_string(),
            ));
        };

        let args: ManageContextToolArgs = serde_json::from_str(&arguments).map_err(|e| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {e:?}"))
        })?;

        let result = handle_manage_context(session.as_ref(), turn.as_ref(), &args).await?;

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&result.json).unwrap_or_else(|_| "{}".to_string()),
            content_items: None,
            success: Some(true),
        })
    }
}

struct ManageContextResult {
    json: serde_json::Value,
}

async fn handle_manage_context(
    session: &Session,
    turn: &TurnContext,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    match args.mode.as_deref() {
        Some("retrieve") => return handle_retrieve(session, turn, args).await,
        Some("apply") => return handle_apply(session, turn, args).await,
        Some("help") => return Ok(ManageContextResult { json: help_json() }),
        Some(other) => {
            return Err(FunctionCallError::RespondToModel(format!(
                "unknown manage_context mode: {other}"
            )));
        }
        None => {}
    }

    if args.include_items.is_some()
        || args.include_notes.is_some()
        || args.include_token_usage.is_some()
        || args.include_pairs.is_some()
        || args.include_internal.is_some()
        || args.max_items.is_some()
        || args.max_top_items.is_some()
        || args.snapshot_id.is_some()
        || !args.ops.is_empty()
        || args.dry_run
        || args.include_prompt_preview.is_some()
        || args.allow_recent.is_some()
    {
        return Err(FunctionCallError::RespondToModel(
            "manage_context requires mode: retrieve | apply | help".to_string(),
        ));
    }

    Ok(ManageContextResult { json: help_json() })
}

fn help_json() -> serde_json::Value {
    json!({
        "mode": "help",
        "summary": [
            "manage_context is an internal tool for the agent to keep long sessions healthy without /compact.",
            "Preferred flow: mode=retrieve (snapshot) then mode=apply (atomic ops) using snapshot_id (anti-drift)."
        ],
        "rules": [
            "replace is allowed ONLY for ToolOutput and Reasoning (never user/assistant messages).",
            "delete is destructive; deleting a tool call also deletes its outputs.",
            "If snapshot_id mismatches, re-run retrieve and retry apply."
        ],
        "tip": "Start with retrieve(include_items=false) to get breakdown + top offenders without bloating context.",
        "example_retrieve": {
            "mode": "retrieve",
            "include_items": false,
        },
        "example_apply": {
            "mode": "apply",
            "snapshot_id": "<from retrieve>",
            "dry_run": true,
            "ops": [
                {"op":"replace","targets":{"call_ids":["call_123"]},"text":"Key results: ..."},
                {"op":"exclude","targets":{"indices":[0,1,2]}},
                {"op":"add_note","notes":["Decision: ...","Constraint: ..."]}
            ]
        }
    })
}

async fn handle_retrieve(
    session: &Session,
    turn: &TurnContext,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    let include_items = args.include_items.unwrap_or(false);
    let include_notes = args.include_notes.unwrap_or(true);
    let include_pairs = args.include_pairs.unwrap_or(true);

    let (context_window, overlay, summaries, items, prompt_items, snapshot_id) = {
        let state = session.state_lock().await;
        let context_window = state
            .token_info()
            .and_then(|info| info.model_context_window)
            .or(turn.client.get_model_context_window());
        let overlay = state.context_overlay_snapshot();
        let ev = state.build_context_items_event();
        let items = state.history_snapshot_lenient();
        let prompt_items = state.prompt_snapshot_lenient();
        let snapshot_id = snapshot_id_for_context(&ev, &overlay, &items);
        (
            context_window,
            overlay,
            ev.items,
            items,
            Some(prompt_items),
            snapshot_id,
        )
    };

    let max_items = args.max_items.unwrap_or(DEFAULT_MAX_ITEMS);
    let slice_start = if include_items {
        summaries.len().saturating_sub(max_items)
    } else {
        summaries.len()
    };

    let include_internal = args.include_internal.unwrap_or(false);
    let max_top_items = args.max_top_items.unwrap_or(DEFAULT_MAX_TOP_ITEMS);

    let breakdown = build_breakdown(
        &summaries,
        &items,
        &overlay,
        max_top_items,
        include_internal,
    );

    let manage_context_call_ids = manage_context_call_ids(&items);
    let tool_name_by_call_id = tool_name_by_call_id(&items);
    let tool_args_preview_by_call_id = tool_args_preview_by_call_id(&items);

    let mut out_items = Vec::new();
    if include_items {
        out_items.reserve(summaries.len().saturating_sub(slice_start));
        for summary in summaries.iter().skip(slice_start) {
            let item = items.get(summary.index);
            if !include_internal && is_manage_context_item(item, &manage_context_call_ids) {
                continue;
            }

            let (call_id, mut tool_name, pair) = describe_pair(item, include_pairs);
            if tool_name.is_none()
                && let Some(cid) = call_id.as_deref()
                && let Some(name) = tool_name_by_call_id.get(cid)
            {
                tool_name = Some((*name).to_string());
            }
            let tool_args_preview = call_id
                .as_deref()
                .and_then(|cid| tool_args_preview_by_call_id.get(cid))
                .cloned();

            let rid = summary.id.as_ref().and_then(|id| parse_rid(id));
            let replacement = rid.and_then(|rid| overlay.replacements_by_rid.get(&rid));
            let raw_bytes = item.map(estimate_item_bytes).unwrap_or(0);
            let effective_bytes = replacement.map(|t| t.len() as u64).unwrap_or(raw_bytes);

            out_items.push(json!({
                "index": summary.index,
                "id": summary.id,
                "category": prune_category_tag(summary.category),
                "included": summary.included,
                "preview": summary.preview,
                "call_id": call_id,
                "tool_name": tool_name,
                "tool_args_preview": tool_args_preview,
                "pair": pair,
                "replaced": replacement.is_some(),
                "effective_preview": replacement.map(|text| preview_text(text)),
                "approx_bytes": {
                    "raw": raw_bytes,
                    "effective": effective_bytes,
                },
            }));
        }
    }

    let (context_window, context_left_percent, tokens_in_context) =
        estimate_token_window(turn, context_window, prompt_items.as_deref().unwrap_or(&[]));
    let token_usage = Some(json!({
        "model_context_window": context_window,
        "tokens_in_context": tokens_in_context,
        "context_left_percent": context_left_percent,
    }));

    Ok(ManageContextResult {
        json: json!({
            "mode": "retrieve",
            "snapshot_id": snapshot_id,
            "token_usage": token_usage,
            "breakdown": breakdown,
            "items": if include_items { Some(out_items) } else { None },
            "notes": if include_notes { Some(overlay.notes) } else { None },
        }),
    })
}

async fn handle_apply(
    session: &Session,
    turn: &TurnContext,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    if args.ops.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "manage_context.apply requires non-empty ops".to_string(),
        ));
    }

    let mut rollout_items: Vec<RolloutItem> = Vec::new();

    let (apply_result, new_snapshot_id, prompt_items, context_window) = {
        let mut state = session.state_lock().await;
        let session_configuration = state.session_configuration.clone();
        let before_ev = state.build_context_items_event();
        let before_overlay = state.context_overlay_snapshot();
        let snapshot_items = state.history_snapshot_lenient();
        let current_snapshot_id =
            snapshot_id_for_context(&before_ev, &before_overlay, &snapshot_items);

        if let Some(expected) = args.snapshot_id.as_deref()
            && expected != current_snapshot_id
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "snapshot mismatch (expected {expected}, got {current_snapshot_id}); run manage_context with mode=retrieve again"
            )));
        }

        let snapshot_rids = state.history_rids_snapshot_lenient();
        let protected_rids = protected_rids_from_context(&before_ev);
        let allow_recent = args.allow_recent.unwrap_or(false);
        let protected_recent_rids = if allow_recent {
            HashSet::new()
        } else {
            recent_message_rids(&snapshot_items, &snapshot_rids)
        };
        let resolved_ops = resolve_ops(
            &snapshot_items,
            &snapshot_rids,
            &args.ops,
            &protected_rids,
            &protected_recent_rids,
        )?;

        if args.dry_run {
            let include_prompt_preview = args.include_prompt_preview.unwrap_or(false);
            let context_window = state
                .token_info()
                .and_then(|info| info.model_context_window)
                .or(turn.client.get_model_context_window());
            let outcome = simulate_apply(
                turn,
                context_window,
                SimulateApplyParams {
                    session_configuration: &session_configuration,
                    snapshot_summaries: &before_ev,
                    snapshot_items: &snapshot_items,
                    snapshot_rids: &snapshot_rids,
                    snapshot_overlay: &before_overlay,
                    ops: &resolved_ops,
                    include_prompt_preview,
                },
            )?;
            let (context_window, context_left_percent, tokens_in_context) = outcome.token_window;
            return Ok(ManageContextResult {
                json: json!({
                    "mode": "apply",
                    "dry_run": true,
                    "ok": true,
                    "applied": outcome.applied,
                    "new_snapshot_id": outcome.new_snapshot_id,
                    "token_usage": {
                        "model_context_window": context_window,
                        "tokens_in_context": tokens_in_context,
                        "context_left_percent": context_left_percent,
                    },
                    "prompt_preview": outcome.prompt_preview,
                }),
            });
        }

        let (summary, include_changed, overlay_changed, deleted_rids) =
            apply_resolved_ops(&mut state, &resolved_ops)?;

        if include_changed || !deleted_rids.is_empty() {
            let after_ev = state.build_context_items_event();
            let (included_indices, included_ids) = included_snapshot(&after_ev);
            let deleted_ids = deleted_rids.into_iter().map(rid_to_string).collect();
            rollout_items.push(RolloutItem::ContextInclusion(ContextInclusionItem {
                included_indices,
                included_ids,
                deleted_indices: Vec::new(),
                deleted_ids,
            }));
        }

        if overlay_changed {
            let overlay_item = context_overlay_rollout_item(&state.context_overlay_snapshot());
            rollout_items.push(overlay_item);
        }

        let final_ev = state.build_context_items_event();
        let final_overlay = state.context_overlay_snapshot();
        let final_items = state.history_snapshot_lenient();
        let new_snapshot_id = snapshot_id_for_context(&final_ev, &final_overlay, &final_items);

        let prompt_items = state.prompt_snapshot_lenient();
        let context_window = state
            .token_info()
            .and_then(|info| info.model_context_window)
            .or(turn.client.get_model_context_window());
        (summary, new_snapshot_id, prompt_items, context_window)
    };

    if !rollout_items.is_empty() {
        session.persist_rollout_items(&rollout_items).await;
    }

    let (context_window, context_left_percent, tokens_in_context) =
        estimate_token_window(turn, context_window, &prompt_items);

    let prompt_preview = if args.include_prompt_preview.unwrap_or(false) {
        Some(prompt_preview_json(&prompt_items))
    } else {
        None
    };

    Ok(ManageContextResult {
        json: json!({
            "mode": "apply",
            "dry_run": false,
            "ok": true,
            "applied": apply_result,
            "new_snapshot_id": new_snapshot_id,
            "token_usage": {
                "model_context_window": context_window,
                "tokens_in_context": tokens_in_context,
                "context_left_percent": context_left_percent,
            },
            "prompt_preview": prompt_preview,
        }),
    })
}

fn estimate_token_window(
    turn_context: &TurnContext,
    context_window: Option<i64>,
    prompt_items: &[ResponseItem],
) -> (Option<i64>, Option<i64>, Option<i64>) {
    let estimated_total_tokens = ({
        let mut history = ContextManager::new();
        history.replace(prompt_items.to_vec());
        history.estimate_token_count(turn_context)
    })
    .unwrap_or(0)
    .max(0);

    let percent_left = context_window.map(|w| {
        TokenUsage {
            input_tokens: estimated_total_tokens,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: estimated_total_tokens,
        }
        .percent_of_context_window_remaining(w)
    });

    (context_window, percent_left, Some(estimated_total_tokens))
}

fn included_snapshot(ev: &ContextItemsEvent) -> (Vec<usize>, Vec<String>) {
    let mut included_indices = Vec::new();
    let mut included_ids = Vec::new();
    for item in &ev.items {
        if item.included {
            included_indices.push(item.index);
            if let Some(id) = &item.id {
                included_ids.push(id.clone());
            }
        }
    }
    (included_indices, included_ids)
}

fn protected_rids_from_context(ev: &ContextItemsEvent) -> HashSet<u64> {
    ev.items
        .iter()
        .filter(|item| is_protected_category(item.category))
        .filter_map(|item| item.id.as_deref().and_then(parse_rid))
        .collect()
}

fn is_protected_category(category: PruneCategory) -> bool {
    matches!(
        category,
        PruneCategory::EnvironmentContext | PruneCategory::UserInstructions
    )
}

fn snapshot_id_for_context(
    ev: &ContextItemsEvent,
    overlay: &ContextOverlay,
    items: &[ResponseItem],
) -> String {
    let last_user_idx = user_message_indices(items).last().copied();
    let manage_context_call_ids: HashSet<&str> = items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                Some(call_id.as_str())
            }
            ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect();

    let mut hasher = sha1::Sha1::new();
    hasher.update(b"items\n");
    for item in &ev.items {
        // Ignore any items appended after the latest user message. During a single "turn", the agent
        // may emit tool calls/outputs and reasoning between `retrieve` and `apply`; those append-only
        // items would otherwise cause immediate `snapshot mismatch` even when the underlying
        // transcript (up to the user message) has not changed.
        if last_user_idx.is_some_and(|cutoff| item.index > cutoff) {
            continue;
        }
        if let Some(raw_item) = items.get(item.index)
            && matches!(
                raw_item,
                ResponseItem::FunctionCall { name, .. } | ResponseItem::CustomToolCall { name, .. }
                    if name == "manage_context"
            )
        {
            continue;
        }
        if let Some(raw_item) = items.get(item.index)
            && matches!(
                raw_item,
                ResponseItem::FunctionCallOutput { call_id, .. }
                    | ResponseItem::CustomToolCallOutput { call_id, .. }
                    if manage_context_call_ids.contains(call_id.as_str())
            )
        {
            continue;
        }

        hasher.update((item.index as u64).to_le_bytes());
        hasher.update(b"\n");
        if let Some(id) = &item.id {
            hasher.update(id.as_bytes());
        }
        hasher.update(b"\n");
        hasher.update(prune_category_tag(item.category).as_bytes());
        hasher.update(b"\n");
        hasher.update([if item.included { 1 } else { 0 }]);
        hasher.update(b"\n");
    }

    hasher.update(b"replacements\n");
    for (rid, text) in &overlay.replacements_by_rid {
        hasher.update(rid.to_le_bytes());
        hasher.update(b"\n");
        hasher.update(text.as_bytes());
        hasher.update(b"\n");
    }

    hasher.update(b"notes\n");
    for note in &overlay.notes {
        hasher.update(note.as_bytes());
        hasher.update(b"\n");
    }

    format!("{:x}", hasher.finalize())
}

fn prune_category_tag(category: PruneCategory) -> &'static str {
    match category {
        PruneCategory::ToolOutput => "tool_output",
        PruneCategory::ToolCall => "tool_call",
        PruneCategory::Reasoning => "reasoning",
        PruneCategory::AssistantMessage => "assistant_message",
        PruneCategory::UserMessage => "user_message",
        PruneCategory::UserInstructions => "user_instructions",
        PruneCategory::EnvironmentContext => "environment_context",
    }
}

#[derive(Default)]
struct CategoryBreakdown {
    total_items: usize,
    included_items: usize,
    raw_bytes_total: u64,
    effective_bytes_total: u64,
    raw_bytes_included: u64,
    effective_bytes_included: u64,
}

#[derive(Default)]
struct CallBreakdown {
    tool_name: Option<String>,
    total_items: usize,
    call_items: usize,
    output_items: usize,
    raw_bytes: u64,
    effective_bytes: u64,
    preview: Option<String>,
    effective_preview: Option<String>,
    replaced: bool,
    max_item_effective_bytes: u64,
}

fn build_breakdown(
    summaries: &[crate::state::ContextItemSummary],
    items: &[ResponseItem],
    overlay: &ContextOverlay,
    max_top_items: usize,
    include_internal: bool,
) -> serde_json::Value {
    let manage_context_call_ids = manage_context_call_ids(items);
    let tool_name_by_call_id = tool_name_by_call_id(items);
    let tool_args_preview_by_call_id = tool_args_preview_by_call_id(items);
    let tool_kind_by_call_id = tool_kind_by_call_id(items);

    let mut by_category: HashMap<PruneCategory, CategoryBreakdown> = HashMap::new();
    let mut top_included: Vec<serde_json::Value> = Vec::new();
    let mut by_call_id: HashMap<&str, CallBreakdown> = HashMap::new();

    for summary in summaries {
        let item = items.get(summary.index);
        if !include_internal && is_manage_context_item(item, &manage_context_call_ids) {
            continue;
        }
        let raw_bytes = item.map(estimate_item_bytes).unwrap_or(0);

        let rid = summary.id.as_ref().and_then(|id| parse_rid(id));
        let replacement = rid.and_then(|rid| overlay.replacements_by_rid.get(&rid));
        let effective_bytes = replacement.map(|t| t.len() as u64).unwrap_or(raw_bytes);

        let entry = by_category.entry(summary.category).or_default();
        entry.total_items += 1;
        entry.raw_bytes_total = entry.raw_bytes_total.saturating_add(raw_bytes);
        entry.effective_bytes_total = entry.effective_bytes_total.saturating_add(effective_bytes);
        if summary.included {
            entry.included_items += 1;
            entry.raw_bytes_included = entry.raw_bytes_included.saturating_add(raw_bytes);
            entry.effective_bytes_included = entry
                .effective_bytes_included
                .saturating_add(effective_bytes);
        }

        if summary.included && max_top_items > 0 {
            let (call_id, mut tool_name, pair) = describe_pair(item, true);
            let tool_args_preview = call_id
                .as_deref()
                .and_then(|cid| tool_args_preview_by_call_id.get(cid))
                .cloned();
            if tool_name.is_none()
                && let Some(cid) = call_id.as_deref()
                && let Some(name) = tool_name_by_call_id.get(cid)
            {
                tool_name = Some((*name).to_string());
            }
            top_included.push(json!({
                "approx_bytes": effective_bytes,
                "index": summary.index,
                "id": summary.id,
                "category": prune_category_tag(summary.category),
                "preview": summary.preview,
                "call_id": call_id,
                "tool_name": tool_name,
                "tool_args_preview": tool_args_preview,
                "pair": pair,
                "replaced": replacement.is_some(),
                "effective_preview": replacement.map(|text| preview_text(text)),
            }));
        }

        if summary.included
            && let Some((call_id, kind)) = item.and_then(call_id_and_kind)
        {
            let entry = by_call_id.entry(call_id).or_default();
            entry.total_items += 1;
            entry.raw_bytes = entry.raw_bytes.saturating_add(raw_bytes);
            entry.effective_bytes = entry.effective_bytes.saturating_add(effective_bytes);
            match kind {
                "call" => entry.call_items += 1,
                "output" => entry.output_items += 1,
                _ => {}
            }

            if entry.tool_name.is_none()
                && let Some(name) = tool_name_by_call_id.get(call_id)
            {
                entry.tool_name = Some((*name).to_string());
            }

            let effective_preview = replacement.map(|text| preview_text(text));
            if replacement.is_some() {
                entry.replaced = true;
            }

            if effective_bytes >= entry.max_item_effective_bytes {
                entry.max_item_effective_bytes = effective_bytes;
                entry.preview = Some(summary.preview.clone());
                entry.effective_preview = effective_preview;
            }
        }
    }

    top_included.sort_by(|a, b| {
        let al = a
            .get("approx_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let bl = b
            .get("approx_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        bl.cmp(&al)
    });
    top_included.truncate(max_top_items);

    let mut top_calls: Vec<serde_json::Value> = by_call_id
        .into_iter()
        .map(|(call_id, v)| {
            let tool_args_preview = tool_args_preview_by_call_id.get(call_id).cloned();
            let kind = tool_kind_by_call_id.get(call_id).map(|v| (*v).to_string());
            json!({
                "call_id": call_id,
                "tool_name": v.tool_name,
                "kind": kind,
                "tool_args_preview": tool_args_preview,
                "approx_bytes": v.effective_bytes,
                "total_items": v.total_items,
                "call_items": v.call_items,
                "output_items": v.output_items,
                "preview": v.preview,
                "effective_preview": v.effective_preview,
                "replaced": v.replaced,
            })
        })
        .collect();
    top_calls.sort_by(|a, b| {
        let al = a
            .get("approx_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let bl = b
            .get("approx_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        bl.cmp(&al)
    });
    top_calls.truncate(max_top_items);

    let mut ordered: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for category in [
        PruneCategory::EnvironmentContext,
        PruneCategory::UserInstructions,
        PruneCategory::UserMessage,
        PruneCategory::AssistantMessage,
        PruneCategory::ToolCall,
        PruneCategory::ToolOutput,
        PruneCategory::Reasoning,
    ] {
        if let Some(stats) = by_category.get(&category) {
            ordered.insert(
                prune_category_tag(category).to_string(),
                json!({
                    "total_items": stats.total_items,
                    "included_items": stats.included_items,
                    "approx_bytes": {
                        "raw_total": stats.raw_bytes_total,
                        "effective_total": stats.effective_bytes_total,
                        "raw_included": stats.raw_bytes_included,
                        "effective_included": stats.effective_bytes_included,
                    }
                }),
            );
        }
    }

    json!({
        "by_category": ordered,
        "top_included_items": top_included,
        "top_calls": top_calls,
    })
}

fn manage_context_call_ids(items: &[ResponseItem]) -> HashSet<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                Some(call_id.as_str())
            }
            ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect()
}

fn is_manage_context_item(
    item: Option<&ResponseItem>,
    manage_context_call_ids: &HashSet<&str>,
) -> bool {
    match item {
        Some(ResponseItem::FunctionCall { name, .. })
        | Some(ResponseItem::CustomToolCall { name, .. }) => name == "manage_context",
        Some(ResponseItem::FunctionCallOutput { call_id, .. })
        | Some(ResponseItem::CustomToolCallOutput { call_id, .. }) => {
            manage_context_call_ids.contains(call_id.as_str())
        }
        _ => false,
    }
}

fn tool_name_by_call_id(items: &[ResponseItem]) -> HashMap<&str, &str> {
    let mut out: HashMap<&str, &str> = HashMap::new();
    for item in items {
        match item {
            ResponseItem::FunctionCall { call_id, name, .. }
            | ResponseItem::CustomToolCall { call_id, name, .. } => {
                out.insert(call_id.as_str(), name.as_str());
            }
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                out.insert(call_id.as_str(), "local_shell");
            }
            _ => {}
        }
    }
    out
}

fn tool_kind_by_call_id(items: &[ResponseItem]) -> HashMap<&str, &'static str> {
    let mut out: HashMap<&str, &'static str> = HashMap::new();
    for item in items {
        match item {
            ResponseItem::FunctionCall { call_id, .. } => {
                out.insert(call_id.as_str(), "function");
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                out.insert(call_id.as_str(), "custom");
            }
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                out.insert(call_id.as_str(), "local_shell");
            }
            _ => {}
        }
    }
    out
}

fn tool_args_preview_by_call_id(items: &[ResponseItem]) -> HashMap<&str, String> {
    let mut out: HashMap<&str, String> = HashMap::new();

    for item in items {
        match item {
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                if let Some(preview) = function_call_args_preview(name, arguments) {
                    out.insert(call_id.as_str(), preview);
                }
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                input,
                ..
            } => {
                if let Some(preview) = custom_tool_call_args_preview(name, input) {
                    out.insert(call_id.as_str(), preview);
                }
            }
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                out.insert(call_id.as_str(), "local_shell".to_string());
            }
            _ => {}
        }
    }

    out
}

fn function_call_args_preview(tool_name: &str, arguments: &str) -> Option<String> {
    let parsed: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(parsed) => parsed,
        Err(_) => return Some(preview_text(arguments)),
    };
    let Some(obj) = parsed.as_object() else {
        return Some(preview_text(arguments));
    };

    match tool_name {
        "exec_command" => {
            let Some(cmd) = obj.get("cmd").and_then(serde_json::Value::as_str) else {
                return Some(preview_text(arguments));
            };
            let mut out = format!("cmd={}", preview_text(cmd));
            if let Some(workdir) = obj.get("workdir").and_then(serde_json::Value::as_str) {
                out.push_str(&format!(" workdir={}", preview_text(workdir)));
            }
            Some(out)
        }
        "shell_command" => {
            let Some(command) = obj.get("command").and_then(serde_json::Value::as_str) else {
                return Some(preview_text(arguments));
            };
            let mut out = format!("command={}", preview_text(command));
            if let Some(workdir) = obj.get("workdir").and_then(serde_json::Value::as_str) {
                out.push_str(&format!(" workdir={}", preview_text(workdir)));
            }
            Some(out)
        }
        "read_file" => {
            let Some(path) = obj.get("file_path").and_then(serde_json::Value::as_str) else {
                return Some(preview_text(arguments));
            };
            let offset = obj.get("offset").and_then(serde_json::Value::as_i64);
            let limit = obj.get("limit").and_then(serde_json::Value::as_i64);

            let mut out = format!("file_path={}", preview_text(path));
            if offset.is_some() || limit.is_some() {
                out.push_str(&format!(
                    " offset={} limit={}",
                    offset.unwrap_or(1),
                    limit.unwrap_or(0)
                ));
            }
            Some(out)
        }
        _ => Some(preview_text(arguments)),
    }
}

fn custom_tool_call_args_preview(tool_name: &str, input: &str) -> Option<String> {
    match tool_name {
        "apply_patch" => Some(format!("patch_bytes={}", input.len())),
        _ => Some(preview_text(input)),
    }
}

fn call_id_and_kind(item: &ResponseItem) -> Option<(&str, &'static str)> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some((call_id.as_str(), "call")),
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some((call_id.as_str(), "output")),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => Some((call_id.as_str(), "call")),
        _ => None,
    }
}

fn estimate_item_bytes(item: &ResponseItem) -> u64 {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let mut total = role.len() as u64;
            for c in content {
                match c {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        total = total.saturating_add(text.len() as u64);
                    }
                    ContentItem::InputImage { image_url } => {
                        total = total.saturating_add(image_url.len() as u64);
                    }
                }
            }
            total
        }
        ResponseItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => {
            let mut total = 0u64;
            for s in summary {
                match s {
                    codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                        total = total.saturating_add(text.len() as u64);
                    }
                }
            }
            if let Some(content) = content {
                for c in content {
                    match c {
                        ReasoningItemContent::ReasoningText { text }
                        | ReasoningItemContent::Text { text } => {
                            total = total.saturating_add(text.len() as u64);
                        }
                    }
                }
            }
            if let Some(enc) = encrypted_content {
                total = total.saturating_add(enc.len() as u64);
            }
            total
        }
        ResponseItem::LocalShellCall {
            call_id, action, ..
        } => {
            let mut total = call_id.as_ref().map(|s| s.len() as u64).unwrap_or(0);
            match action {
                codex_protocol::models::LocalShellAction::Exec(exec) => {
                    for part in &exec.command {
                        total = total.saturating_add(part.len() as u64);
                    }
                    if let Some(wd) = &exec.working_directory {
                        total = total.saturating_add(wd.len() as u64);
                    }
                    if let Some(env) = &exec.env {
                        for (k, v) in env {
                            total = total.saturating_add(k.len() as u64);
                            total = total.saturating_add(v.len() as u64);
                        }
                    }
                }
            }
            total
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => (name.len() + arguments.len() + call_id.len()) as u64,
        ResponseItem::FunctionCallOutput { call_id, output } => {
            let mut total = (call_id.len() + output.content.len()) as u64;
            if let Some(items) = &output.content_items {
                for it in items {
                    match it {
                        codex_protocol::models::FunctionCallOutputContentItem::InputText {
                            text,
                        } => {
                            total = total.saturating_add(text.len() as u64);
                        }
                        codex_protocol::models::FunctionCallOutputContentItem::InputImage {
                            image_url,
                        } => {
                            total = total.saturating_add(image_url.len() as u64);
                        }
                    }
                }
            }
            total
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => (call_id.len() + name.len() + input.len()) as u64,
        ResponseItem::CustomToolCallOutput { call_id, output } => {
            (call_id.len() + output.len()) as u64
        }
        ResponseItem::WebSearchCall { action, .. } => match action {
            codex_protocol::models::WebSearchAction::Search { query } => {
                query.as_ref().map(|s| s.len() as u64).unwrap_or(0)
            }
            codex_protocol::models::WebSearchAction::OpenPage { url } => {
                url.as_ref().map(|s| s.len() as u64).unwrap_or(0)
            }
            codex_protocol::models::WebSearchAction::FindInPage { url, pattern } => {
                url.as_ref().map(|s| s.len() as u64).unwrap_or(0)
                    + pattern.as_ref().map(|s| s.len() as u64).unwrap_or(0)
            }
            codex_protocol::models::WebSearchAction::Other => 0,
        },
        ResponseItem::Compaction { encrypted_content } => encrypted_content.len() as u64,
        ResponseItem::GhostSnapshot { .. } | ResponseItem::Other => 0,
    }
}

fn describe_pair(
    item: Option<&ResponseItem>,
    include_pairs: bool,
) -> (Option<String>, Option<String>, Option<serde_json::Value>) {
    let Some(item) = item else {
        return (None, None, None);
    };

    let (call_id, tool_name, pair_kind, pair_call_id) = match item {
        ResponseItem::FunctionCall { call_id, name, .. } => (
            Some(call_id.clone()),
            Some(name.clone()),
            Some("call"),
            Some(call_id.clone()),
        ),
        ResponseItem::FunctionCallOutput { call_id, .. } => (
            Some(call_id.clone()),
            None,
            Some("output"),
            Some(call_id.clone()),
        ),
        ResponseItem::CustomToolCall { call_id, name, .. } => (
            Some(call_id.clone()),
            Some(name.clone()),
            Some("call"),
            Some(call_id.clone()),
        ),
        ResponseItem::CustomToolCallOutput { call_id, .. } => (
            Some(call_id.clone()),
            None,
            Some("output"),
            Some(call_id.clone()),
        ),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => (
            Some(call_id.clone()),
            Some("local_shell".to_string()),
            Some("call"),
            Some(call_id.clone()),
        ),
        _ => (None, None, None, None),
    };

    let pair = if include_pairs {
        pair_kind.map(|kind| {
            json!({
                "kind": kind,
                "pair_call_id": pair_call_id,
            })
        })
    } else {
        None
    };

    (call_id, tool_name, pair)
}

fn preview_text(text: &str) -> String {
    const MAX: usize = 80;
    preview_text_lines(text, 1, MAX)
}

fn preview_text_lines(text: &str, max_lines: usize, max_chars: usize) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in text.trim().lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        parts.push(trimmed);
        if parts.len() >= max_lines {
            break;
        }
    }

    let joined = parts.join(" | ");
    if joined.len() <= max_chars {
        return joined;
    }

    let slice = codex_utils_string::take_bytes_at_char_boundary(&joined, max_chars);
    if slice.len() < joined.len() {
        format!("{slice}…")
    } else {
        slice.to_string()
    }
}

fn prompt_preview_json(items: &[ResponseItem]) -> serde_json::Value {
    const MAX_CHARS: usize = 8_000;
    const MAX_ITEMS: usize = 200;
    const MAX_ITEM_LINES: usize = 4;
    const MAX_ITEM_CHARS: usize = 200;

    let mut out = String::new();
    let mut shown_items = 0usize;
    let mut truncated = false;

    let iter = items.iter().take(MAX_ITEMS);
    for item in iter {
        let line = prompt_preview_line(item, MAX_ITEM_LINES, MAX_ITEM_CHARS);
        if line.is_empty() {
            continue;
        }

        let sep = if out.is_empty() { "" } else { "\n" };
        let new_len = out
            .len()
            .saturating_add(sep.len())
            .saturating_add(line.len());
        if new_len > MAX_CHARS {
            truncated = true;
            let remaining = MAX_CHARS.saturating_sub(out.len().saturating_add(sep.len()));
            if remaining > 1 {
                out.push_str(sep);
                let slice = codex_utils_string::take_bytes_at_char_boundary(&line, remaining - 1);
                out.push_str(slice);
                out.push('…');
                shown_items += 1;
            }
            break;
        }

        out.push_str(sep);
        out.push_str(&line);
        shown_items += 1;
    }

    if items.len() > MAX_ITEMS {
        truncated = true;
    }

    json!({
        "text": out,
        "truncated": truncated,
        "total_items": items.len(),
        "shown_items": shown_items,
    })
}

fn prompt_preview_line(item: &ResponseItem, max_lines: usize, max_chars: usize) -> String {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let text = first_text(content).unwrap_or("");
            let preview = preview_text_lines(text, max_lines, max_chars);
            if preview.is_empty() {
                role.to_string()
            } else {
                format!("{role}: {preview}")
            }
        }
        ResponseItem::Reasoning { summary, .. } => {
            let text = summary.first().map_or("reasoning", |s| match s {
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                    text.as_str()
                }
            });
            format!(
                "reasoning: {}",
                preview_text_lines(text, max_lines, max_chars)
            )
        }
        ResponseItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => {
            let args_preview = function_call_args_preview(name, arguments).unwrap_or_default();
            if args_preview.is_empty() {
                format!("tool call ({call_id}): {name}")
            } else {
                format!("tool call ({call_id}): {name} {args_preview}")
            }
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            let preview_line = tool_output_preview_line(&output.content);
            format!(
                "tool output ({call_id}): {}",
                preview_text_lines(preview_line, max_lines, max_chars)
            )
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => {
            let args_preview = custom_tool_call_args_preview(name, input).unwrap_or_default();
            if args_preview.is_empty() {
                format!("tool call ({call_id}): {name}")
            } else {
                format!("tool call ({call_id}): {name} {args_preview}")
            }
        }
        ResponseItem::CustomToolCallOutput { call_id, output } => {
            let preview_line = tool_output_preview_line(output);
            format!(
                "tool output ({call_id}): {}",
                preview_text_lines(preview_line, max_lines, max_chars)
            )
        }
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => format!("tool call ({call_id}): local_shell"),
        ResponseItem::LocalShellCall { call_id: None, .. } => "tool call: local_shell".to_string(),
        ResponseItem::WebSearchCall { action, .. } => {
            use codex_protocol::models::WebSearchAction;
            match action {
                WebSearchAction::Search { query } => format!(
                    "web_search: {}",
                    query
                        .as_deref()
                        .map(|q| preview_text_lines(q, max_lines, max_chars))
                        .unwrap_or_else(|| "search".to_string())
                ),
                WebSearchAction::OpenPage { url } => format!(
                    "web_search: open {}",
                    url.as_deref()
                        .map(|u| preview_text_lines(u, max_lines, max_chars))
                        .unwrap_or_default()
                ),
                WebSearchAction::FindInPage { url, pattern } => format!(
                    "web_search: find {} {}",
                    url.as_deref()
                        .map(|u| preview_text_lines(u, max_lines, max_chars))
                        .unwrap_or_default(),
                    pattern
                        .as_deref()
                        .map(|p| preview_text_lines(p, max_lines, max_chars))
                        .unwrap_or_default()
                ),
                WebSearchAction::Other => "web_search".to_string(),
            }
        }
        ResponseItem::Compaction { encrypted_content } => {
            format!("compaction ({} bytes)", encrypted_content.len())
        }
        ResponseItem::GhostSnapshot { .. } => "ghost snapshot".to_string(),
        ResponseItem::Other => String::new(),
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

fn context_overlay_rollout_item(overlay: &ContextOverlay) -> RolloutItem {
    let replacements: Vec<ContextOverlayReplacement> = overlay
        .replacements_by_rid
        .iter()
        .map(|(rid, text)| ContextOverlayReplacement {
            id: rid_to_string(*rid),
            text: text.clone(),
        })
        .collect();
    RolloutItem::ContextOverlay(ContextOverlayItem {
        replacements,
        notes: overlay.notes.clone(),
    })
}

fn resolve_ops(
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    ops: &[ManageContextOp],
    protected_rids: &HashSet<u64>,
    protected_recent_rids: &HashSet<u64>,
) -> Result<Vec<ResolvedOp>, FunctionCallError> {
    let mut resolved = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        let op_index = idx + 1;
        match op.op.as_str() {
            "include" | "exclude" | "delete" | "replace" | "clear_replace" => {
                let targets = op.targets.as_ref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(format!(
                        "op {op_index} ({}) requires targets",
                        op.op
                    ))
                })?;
                let rids = match op.op.as_str() {
                    "replace" | "clear_replace" => {
                        resolve_target_rids_for_replace(snapshot_items, snapshot_rids, targets)
                    }
                    _ => resolve_target_rids(snapshot_items, snapshot_rids, targets),
                };
                if rids.is_empty() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "op {op_index} ({}) resolved to 0 targets",
                        op.op
                    )));
                }
                if matches!(op.op.as_str(), "include" | "exclude" | "delete") {
                    let forbidden = rids
                        .iter()
                        .copied()
                        .filter(|rid| protected_rids.contains(rid))
                        .map(rid_to_string)
                        .collect::<Vec<_>>();
                    if !forbidden.is_empty() {
                        return Err(FunctionCallError::RespondToModel(format!(
                            "op {op_index} ({}) targets protected context item(s): {}",
                            op.op,
                            forbidden.join(", ")
                        )));
                    }
                }
                if matches!(op.op.as_str(), "exclude" | "delete") {
                    validate_recent_rids(op_index, &op.op, &rids, protected_recent_rids)?;
                }

                match op.op.as_str() {
                    "include" => resolved.push(ResolvedOp::Include { rids }),
                    "exclude" => resolved.push(ResolvedOp::Exclude { rids }),
                    "delete" => {
                        let cascade = op.cascade.as_deref().unwrap_or("tool_outputs");
                        if cascade != "tool_outputs" {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "op {op_index} (delete) only supports cascade=tool_outputs"
                            )));
                        }
                        resolved.push(ResolvedOp::Delete { rids });
                    }
                    "replace" => {
                        let Some(text) = op.text.as_deref() else {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "op {op_index} (replace) requires text"
                            )));
                        };
                        if text.trim().is_empty() {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "op {op_index} (replace) requires non-empty text"
                            )));
                        }
                        for rid in &rids {
                            if let Some(pos) = snapshot_rids.iter().position(|r| r == rid)
                                && let Some(item) = snapshot_items.get(pos)
                                && !matches!(
                                    item,
                                    ResponseItem::FunctionCallOutput { .. }
                                        | ResponseItem::CustomToolCallOutput { .. }
                                        | ResponseItem::Reasoning { .. }
                                )
                            {
                                return Err(FunctionCallError::RespondToModel(format!(
                                    "op {op_index} (replace) only supports tool outputs and reasoning (id={})",
                                    rid_to_string(*rid)
                                )));
                            }
                        }
                        resolved.push(ResolvedOp::Replace {
                            rids,
                            text: text.to_string(),
                        });
                    }
                    "clear_replace" => resolved.push(ResolvedOp::ClearReplace { rids }),
                    _ => unreachable!(),
                }
            }
            "clear_replace_all" => resolved.push(ResolvedOp::ClearReplaceAll),
            "include_all" => resolved.push(ResolvedOp::IncludeAll),
            "add_note" => {
                if op.notes.is_empty() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "op {op_index} (add_note) requires non-empty notes"
                    )));
                }
                resolved.push(ResolvedOp::AddNote {
                    notes: op.notes.clone(),
                });
            }
            "remove_note" => {
                if op.note_indices.is_empty() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "op {op_index} (remove_note) requires note_indices"
                    )));
                }
                resolved.push(ResolvedOp::RemoveNote {
                    note_indices: op.note_indices.clone(),
                });
            }
            "clear_notes" => resolved.push(ResolvedOp::ClearNotes),
            other => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unknown manage_context op: {other}"
                )));
            }
        }
    }

    Ok(resolved)
}

fn validate_recent_rids(
    op_index: usize,
    op_name: &str,
    rids: &[u64],
    protected_recent_rids: &HashSet<u64>,
) -> Result<(), FunctionCallError> {
    if protected_recent_rids.is_empty() {
        return Ok(());
    }

    let blocked = rids
        .iter()
        .copied()
        .filter(|rid| protected_recent_rids.contains(rid))
        .map(rid_to_string)
        .collect::<Vec<_>>();

    if blocked.is_empty() {
        return Ok(());
    }

    Err(FunctionCallError::RespondToModel(format!(
        "op {op_index} ({op_name}) targets recent message id(s): {}; set allow_recent=true to override",
        blocked.join(", ")
    )))
}

fn recent_message_rids(snapshot_items: &[ResponseItem], snapshot_rids: &[u64]) -> HashSet<u64> {
    let mut out: HashSet<u64> = HashSet::new();

    if let Some(last_user_idx) = user_message_indices(snapshot_items).last().copied()
        && let Some(rid) = snapshot_rids.get(last_user_idx).copied()
    {
        out.insert(rid);
    }

    if let Some(last_assistant_idx) = snapshot_items
        .iter()
        .rposition(|item| matches!(item, ResponseItem::Message { role, .. } if role == "assistant"))
        && let Some(rid) = snapshot_rids.get(last_assistant_idx).copied()
    {
        out.insert(rid);
    }

    out
}

fn user_message_indices(items: &[ResponseItem]) -> Vec<usize> {
    let mut out = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        if role != "user" {
            continue;
        }
        if UserInstructions::is_user_instructions(content)
            || SkillInstructions::is_skill_instructions(content)
        {
            continue;
        }
        if is_environment_context_message(content) {
            continue;
        }
        out.push(idx);
    }
    out
}

fn is_environment_context_message(content: &[ContentItem]) -> bool {
    let Some(text) = first_text(content) else {
        return false;
    };
    starts_with_case_insensitive(text.trim(), ENVIRONMENT_CONTEXT_OPEN_TAG)
}

fn first_text(content: &[ContentItem]) -> Option<&str> {
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                return Some(text);
            }
            ContentItem::InputImage { .. } => {}
        }
    }
    None
}

fn starts_with_case_insensitive(text: &str, prefix: &str) -> bool {
    let pl = prefix.len();
    match text.get(..pl) {
        Some(head) => head.eq_ignore_ascii_case(prefix),
        None => false,
    }
}

fn resolve_target_rids(
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    targets: &ManageContextTargets,
) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();

    let mut call_set: HashSet<String> = targets.call_ids.iter().cloned().collect();

    for &idx in &targets.indices {
        if let Some(rid) = snapshot_rids.get(idx).copied() {
            out.push(rid);
        }
    }

    for raw in &targets.ids {
        if let Some(rid) = parse_rid(raw) {
            out.push(rid);
        }
    }

    if !out.is_empty() {
        let mut rid_lookup: HashMap<u64, usize> = HashMap::new();
        for (idx, rid) in snapshot_rids.iter().copied().enumerate() {
            rid_lookup.insert(rid, idx);
        }

        for rid in &out {
            let Some(idx) = rid_lookup.get(rid).copied() else {
                continue;
            };
            let Some(item) = snapshot_items.get(idx) else {
                continue;
            };

            match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::CustomToolCall { call_id, .. }
                | ResponseItem::CustomToolCallOutput { call_id, .. } => {
                    call_set.insert(call_id.clone());
                }
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => {
                    call_set.insert(call_id.clone());
                }
                _ => {}
            }
        }
    }

    if !call_set.is_empty() {
        for (idx, item) in snapshot_items.iter().enumerate() {
            let call_id = match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::CustomToolCall { call_id, .. }
                | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => Some(call_id.as_str()),
                _ => None,
            };
            if call_id.is_some_and(|cid| call_set.contains(cid))
                && let Some(rid) = snapshot_rids.get(idx).copied()
            {
                out.push(rid);
            }
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ContextItemSummary;
    use pretty_assertions::assert_eq;

    fn user_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
        }
    }

    fn assistant_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
        }
    }

    fn reasoning(text: &str) -> ResponseItem {
        ResponseItem::Reasoning {
            id: String::new(),
            summary: vec![
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText {
                    text: text.to_string(),
                },
            ],
            content: None,
            encrypted_content: None,
        }
    }

    fn manage_context_call(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "manage_context".to_string(),
            arguments: "{}".to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn manage_context_custom_call(call_id: &str) -> ResponseItem {
        ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: call_id.to_string(),
            name: "manage_context".to_string(),
            input: "{}".to_string(),
        }
    }

    fn function_call_output(call_id: &str, content: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                content: content.to_string(),
                content_items: None,
                success: Some(true),
            },
        }
    }

    fn custom_tool_call_output(call_id: &str, output: &str) -> ResponseItem {
        ResponseItem::CustomToolCallOutput {
            call_id: call_id.to_string(),
            output: output.to_string(),
        }
    }

    #[tokio::test]
    async fn manage_context_no_args_returns_help() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let args: ManageContextToolArgs = serde_json::from_str("{}").expect("parse args");
        let result = handle_manage_context(&session, &turn, &args)
            .await
            .expect("manage_context");
        assert_eq!(
            result.json.get("mode").and_then(|v| v.as_str()),
            Some("help")
        );
    }

    #[tokio::test]
    async fn manage_context_apply_returns_token_usage() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let args: ManageContextToolArgs = serde_json::from_str(
            r#"{"mode":"apply","dry_run":true,"ops":[{"op":"add_note","notes":["x"]}]}"#,
        )
        .expect("parse args");
        let result = handle_manage_context(&session, &turn, &args)
            .await
            .expect("manage_context");

        assert_eq!(
            result.json.get("mode").and_then(|v| v.as_str()),
            Some("apply")
        );
        assert_eq!(
            result
                .json
                .get("dry_run")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            result
                .json
                .get("token_usage")
                .is_some_and(serde_json::Value::is_object),
            "apply should return token_usage"
        );
        assert!(
            result
                .json
                .pointer("/applied/affected_call_ids")
                .is_some_and(serde_json::Value::is_object),
            "apply should return applied.affected_call_ids"
        );
    }

    #[tokio::test]
    async fn manage_context_apply_prompt_preview_when_requested() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let args: ManageContextToolArgs = serde_json::from_str(
            r#"{"mode":"apply","dry_run":true,"include_prompt_preview":true,"ops":[{"op":"add_note","notes":["x"]}]}"#,
        )
        .expect("parse args");
        let result = handle_manage_context(&session, &turn, &args)
            .await
            .expect("manage_context");

        let preview = result.json.get("prompt_preview").expect("prompt_preview");
        let text = preview
            .get("text")
            .and_then(serde_json::Value::as_str)
            .expect("prompt_preview.text");

        assert!(
            text.contains("Pinned notes:") && text.contains("- x"),
            "prompt preview should include the pinned note"
        );
    }

    #[tokio::test]
    async fn manage_context_refuses_excluding_recent_without_allow_recent() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        {
            let mut state = session.state_lock().await;
            state.record_items(
                [&user_message("u1"), &assistant_message("a1")],
                turn.truncation_policy,
            );
        }

        let args: ManageContextToolArgs = serde_json::from_str(
            r#"{"mode":"apply","dry_run":true,"ops":[{"op":"exclude","targets":{"indices":[0]}}]}"#,
        )
        .expect("parse args");
        let err = match handle_manage_context(&session, &turn, &args).await {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected RespondToModel error");
        };
        assert!(
            message.contains("allow_recent=true"),
            "error should mention allow_recent=true override"
        );

        let args: ManageContextToolArgs = serde_json::from_str(
            r#"{"mode":"apply","dry_run":true,"allow_recent":true,"ops":[{"op":"exclude","targets":{"indices":[0]}}]}"#,
        )
        .expect("parse args");
        let ok = handle_manage_context(&session, &turn, &args)
            .await
            .expect("manage_context");
        assert_eq!(ok.json.get("mode").and_then(|v| v.as_str()), Some("apply"));
    }

    #[test]
    fn snapshot_id_ignores_manage_context_call_and_output() {
        let overlay = ContextOverlay::default();

        let items = vec![user_message("u1"), user_message("u2")];
        let ev = ContextItemsEvent {
            items: vec![
                ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
            ],
        };
        let base = snapshot_id_for_context(&ev, &overlay, &items);

        let call_id = "call_manage_context";
        let items_with_manage_context = vec![
            user_message("u1"),
            user_message("u2"),
            manage_context_call(call_id),
            function_call_output(call_id, "{\"ok\":true}"),
        ];
        let ev_with_manage_context = ContextItemsEvent {
            items: vec![
                ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                ContextItemSummary {
                    index: 2,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
                ContextItemSummary {
                    index: 3,
                    category: PruneCategory::ToolOutput,
                    preview: String::new(),
                    included: true,
                    id: Some("r3".to_string()),
                },
            ],
        };
        let extended = snapshot_id_for_context(
            &ev_with_manage_context,
            &overlay,
            &items_with_manage_context,
        );

        assert_eq!(base, extended);
    }

    #[test]
    fn breakdown_hides_manage_context_by_default() {
        let manage_context_output = "x".repeat(500);
        let items = vec![
            manage_context_call("call_manage_context"),
            function_call_output("call_manage_context", &manage_context_output),
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".to_string(),
                arguments: r#"{"cmd":"echo hi"}"#.to_string(),
                call_id: "call_exec".to_string(),
            },
            function_call_output("call_exec", "ok"),
        ];

        let summaries = vec![
            ContextItemSummary {
                index: 0,
                category: PruneCategory::ToolCall,
                preview: String::new(),
                included: true,
                id: Some("r0".to_string()),
            },
            ContextItemSummary {
                index: 1,
                category: PruneCategory::ToolOutput,
                preview: String::new(),
                included: true,
                id: Some("r1".to_string()),
            },
            ContextItemSummary {
                index: 2,
                category: PruneCategory::ToolCall,
                preview: String::new(),
                included: true,
                id: Some("r2".to_string()),
            },
            ContextItemSummary {
                index: 3,
                category: PruneCategory::ToolOutput,
                preview: String::new(),
                included: true,
                id: Some("r3".to_string()),
            },
        ];

        let mut overlay = ContextOverlay::default();
        overlay
            .replacements_by_rid
            .insert(3, "replacement".to_string());

        let breakdown = build_breakdown(&summaries, &items, &overlay, 10, false);
        let top = breakdown
            .get("top_included_items")
            .and_then(serde_json::Value::as_array)
            .expect("top_included_items array");
        assert!(
            top.iter()
                .all(|it| it.get("call_id").and_then(|v| v.as_str()) != Some("call_manage_context")),
            "manage_context call/output should be hidden by default"
        );

        let top_calls = breakdown
            .get("top_calls")
            .and_then(serde_json::Value::as_array)
            .expect("top_calls array");
        assert!(
            top_calls
                .iter()
                .all(|it| it.get("call_id").and_then(|v| v.as_str()) != Some("call_manage_context")),
            "top_calls should hide manage_context by default"
        );
    }

    #[test]
    fn breakdown_respects_max_top_items_and_includes_effective_preview() {
        let items = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".to_string(),
                arguments: r#"{"cmd":"echo hi"}"#.to_string(),
                call_id: "call_exec".to_string(),
            },
            function_call_output("call_exec", "small"),
            ResponseItem::FunctionCall {
                id: None,
                name: "grep_files".to_string(),
                arguments: r#"{"pattern":"x"}"#.to_string(),
                call_id: "call_grep".to_string(),
            },
            function_call_output("call_grep", "also small"),
        ];

        let summaries = vec![
            ContextItemSummary {
                index: 0,
                category: PruneCategory::ToolCall,
                preview: String::new(),
                included: true,
                id: Some("r0".to_string()),
            },
            ContextItemSummary {
                index: 1,
                category: PruneCategory::ToolOutput,
                preview: String::new(),
                included: true,
                id: Some("r1".to_string()),
            },
            ContextItemSummary {
                index: 2,
                category: PruneCategory::ToolCall,
                preview: String::new(),
                included: true,
                id: Some("r2".to_string()),
            },
            ContextItemSummary {
                index: 3,
                category: PruneCategory::ToolOutput,
                preview: String::new(),
                included: true,
                id: Some("r3".to_string()),
            },
        ];

        let mut overlay = ContextOverlay::default();
        overlay
            .replacements_by_rid
            .insert(1, format!("effective preview\n{}", "x".repeat(200)));

        let breakdown = build_breakdown(&summaries, &items, &overlay, 1, true);
        let top = breakdown
            .get("top_included_items")
            .and_then(serde_json::Value::as_array)
            .expect("top_included_items array");
        assert_eq!(top.len(), 1);

        let entry = &top[0];
        assert_eq!(
            entry.get("effective_preview").and_then(|v| v.as_str()),
            Some("effective preview")
        );
        assert_eq!(
            entry.get("replaced").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            entry.get("tool_name").and_then(|v| v.as_str()),
            Some("exec_command")
        );
        assert_eq!(
            entry.get("tool_args_preview").and_then(|v| v.as_str()),
            Some("cmd=echo hi")
        );
        assert!(
            entry.get("pair").is_some_and(serde_json::Value::is_object),
            "pair should be present"
        );

        let top_calls = breakdown
            .get("top_calls")
            .and_then(serde_json::Value::as_array)
            .expect("top_calls array");
        assert_eq!(top_calls.len(), 1);
        assert_eq!(
            top_calls[0].get("call_id").and_then(|v| v.as_str()),
            Some("call_exec")
        );
        assert_eq!(
            top_calls[0]
                .get("tool_args_preview")
                .and_then(|v| v.as_str()),
            Some("cmd=echo hi")
        );
    }

    #[test]
    fn snapshot_id_ignores_manage_context_custom_call_and_output() {
        let overlay = ContextOverlay::default();

        let items = vec![user_message("u1"), user_message("u2")];
        let ev = ContextItemsEvent {
            items: vec![
                ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
            ],
        };
        let base = snapshot_id_for_context(&ev, &overlay, &items);

        let call_id = "call_manage_context_custom";
        let items_with_manage_context = vec![
            user_message("u1"),
            user_message("u2"),
            manage_context_custom_call(call_id),
            custom_tool_call_output(call_id, "{\"ok\":true}"),
        ];
        let ev_with_manage_context = ContextItemsEvent {
            items: vec![
                ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                ContextItemSummary {
                    index: 2,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
                ContextItemSummary {
                    index: 3,
                    category: PruneCategory::ToolOutput,
                    preview: String::new(),
                    included: true,
                    id: Some("r3".to_string()),
                },
            ],
        };
        let extended = snapshot_id_for_context(
            &ev_with_manage_context,
            &overlay,
            &items_with_manage_context,
        );

        assert_eq!(base, extended);
    }

    #[test]
    fn snapshot_id_ignores_items_after_last_user_message() {
        let overlay = ContextOverlay::default();

        let items = vec![user_message("u1"), user_message("u2")];
        let ev = ContextItemsEvent {
            items: vec![
                ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
            ],
        };
        let base = snapshot_id_for_context(&ev, &overlay, &items);

        let items_with_trailing = vec![user_message("u1"), user_message("u2"), reasoning("r1")];
        let ev_with_trailing = ContextItemsEvent {
            items: vec![
                ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                ContextItemSummary {
                    index: 2,
                    category: PruneCategory::Reasoning,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
            ],
        };
        let extended = snapshot_id_for_context(&ev_with_trailing, &overlay, &items_with_trailing);

        assert_eq!(base, extended);
    }
}

fn resolve_target_rids_for_replace(
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    targets: &ManageContextTargets,
) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();

    for &idx in &targets.indices {
        if let Some(rid) = snapshot_rids.get(idx).copied() {
            out.push(rid);
        }
    }

    for raw in &targets.ids {
        if let Some(rid) = parse_rid(raw) {
            out.push(rid);
        }
    }

    if !targets.call_ids.is_empty() {
        let call_set: HashSet<&str> = targets.call_ids.iter().map(String::as_str).collect();
        for (idx, item) in snapshot_items.iter().enumerate() {
            let call_id = match item {
                ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            };
            if call_id.is_some_and(|cid| call_set.contains(cid))
                && let Some(rid) = snapshot_rids.get(idx).copied()
            {
                out.push(rid);
            }
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

#[derive(Debug)]
enum ResolvedOp {
    Include { rids: Vec<u64> },
    Exclude { rids: Vec<u64> },
    IncludeAll,
    Delete { rids: Vec<u64> },
    Replace { rids: Vec<u64>, text: String },
    ClearReplace { rids: Vec<u64> },
    ClearReplaceAll,
    AddNote { notes: Vec<String> },
    RemoveNote { note_indices: Vec<usize> },
    ClearNotes,
}

struct SimulateApplyParams<'a> {
    session_configuration: &'a crate::codex::SessionConfiguration,
    snapshot_summaries: &'a ContextItemsEvent,
    snapshot_items: &'a [ResponseItem],
    snapshot_rids: &'a [u64],
    snapshot_overlay: &'a ContextOverlay,
    ops: &'a [ResolvedOp],
    include_prompt_preview: bool,
}

struct SimulateApplyOutcome {
    applied: serde_json::Value,
    new_snapshot_id: String,
    token_window: (Option<i64>, Option<i64>, Option<i64>),
    prompt_preview: Option<serde_json::Value>,
}

fn simulate_apply(
    turn_context: &TurnContext,
    context_window: Option<i64>,
    params: SimulateApplyParams<'_>,
) -> Result<SimulateApplyOutcome, FunctionCallError> {
    let mut temp = crate::state::SessionState::new(params.session_configuration.clone());
    temp.replace_history_with_rids(
        params.snapshot_items.to_vec(),
        params.snapshot_rids.to_vec(),
    );

    let mut included: BTreeSet<usize> = BTreeSet::new();
    for item in &params.snapshot_summaries.items {
        if item.included {
            included.insert(item.index);
        }
    }
    temp.set_include_mask_from_rids(Some(included), params.snapshot_rids);
    temp.set_context_overlay(params.snapshot_overlay.clone());

    let (summary, _include_changed, _overlay_changed, _deleted_rids) =
        apply_resolved_ops(&mut temp, params.ops)?;

    let after_ev = temp.build_context_items_event();
    let after_overlay = temp.context_overlay_snapshot();
    let after_items = temp.history_snapshot_lenient();
    let new_snapshot_id = snapshot_id_for_context(&after_ev, &after_overlay, &after_items);

    let prompt_items = temp.prompt_snapshot_lenient();
    let token_window = estimate_token_window(turn_context, context_window, &prompt_items);
    let prompt_preview = params
        .include_prompt_preview
        .then(|| prompt_preview_json(&prompt_items));

    Ok(SimulateApplyOutcome {
        applied: summary,
        new_snapshot_id,
        token_window,
        prompt_preview,
    })
}

fn apply_resolved_ops(
    state: &mut crate::state::SessionState,
    ops: &[ResolvedOp],
) -> Result<(serde_json::Value, bool, bool, Vec<u64>), FunctionCallError> {
    let call_id_by_rid = {
        let (snapshot_items, snapshot_rids) = state.history_snapshot_with_rids_lenient();
        call_id_by_rid(&snapshot_items, &snapshot_rids)
    };

    let mut include_changed = false;
    let mut overlay_changed = false;
    let mut deleted_rids: Vec<u64> = Vec::new();
    let mut skipped_missing_targets = 0usize;
    let mut missing_rids: Vec<u64> = Vec::new();
    let mut affected_rids: Vec<u64> = Vec::new();

    let mut count_included = 0usize;
    let mut count_excluded = 0usize;
    let mut count_deleted = 0usize;
    let mut count_replaced = 0usize;
    let mut count_cleared_replacements = 0usize;
    let mut count_notes_added = 0usize;
    let mut count_notes_removed = 0usize;
    let mut cleared_notes = false;

    for op in ops {
        match op {
            ResolvedOp::Include { rids } => {
                let indices =
                    indices_for_rids(state, rids, &mut skipped_missing_targets, &mut missing_rids);
                if !indices.is_empty() {
                    let snapshot_rids = state.history_rids_snapshot_lenient();
                    for idx in &indices {
                        if let Some(rid) = snapshot_rids.get(*idx).copied() {
                            affected_rids.push(rid);
                        }
                    }
                    state.set_context_inclusion(&indices, true);
                    include_changed = true;
                    count_included += indices.len();
                }
            }
            ResolvedOp::Exclude { rids } => {
                let indices =
                    indices_for_rids(state, rids, &mut skipped_missing_targets, &mut missing_rids);
                if !indices.is_empty() {
                    let snapshot_rids = state.history_rids_snapshot_lenient();
                    for idx in &indices {
                        if let Some(rid) = snapshot_rids.get(*idx).copied() {
                            affected_rids.push(rid);
                        }
                    }
                    state.set_context_inclusion(&indices, false);
                    include_changed = true;
                    count_excluded += indices.len();
                }
            }
            ResolvedOp::IncludeAll => {
                state.set_include_mask(None);
                include_changed = true;
            }
            ResolvedOp::Delete { rids } => {
                let indices =
                    indices_for_rids(state, rids, &mut skipped_missing_targets, &mut missing_rids);
                if !indices.is_empty() {
                    let res = state.prune_by_indices_lenient(indices);
                    count_deleted += res.deleted_indices.len();
                    deleted_rids.extend(res.deleted_rids.iter().copied());
                    affected_rids.extend(res.deleted_rids);
                    include_changed = true;
                }
            }
            ResolvedOp::Replace { rids, text } => {
                let current_rids: HashSet<u64> =
                    state.history_rids_snapshot_lenient().into_iter().collect();
                let mut updates = Vec::new();
                for rid in rids {
                    if current_rids.contains(rid) {
                        updates.push((*rid, text.clone()));
                    } else {
                        skipped_missing_targets += 1;
                        missing_rids.push(*rid);
                    }
                }
                if !updates.is_empty() {
                    count_replaced += updates.len();
                    affected_rids.extend(updates.iter().map(|(rid, _)| *rid));
                    state.upsert_context_replacements(updates);
                    overlay_changed = true;
                }
            }
            ResolvedOp::ClearReplace { rids } => {
                let before = state.context_overlay_snapshot();
                state.clear_context_replacements_for(rids);
                let after = state.context_overlay_snapshot();
                let cleared = before
                    .replacements_by_rid
                    .len()
                    .saturating_sub(after.replacements_by_rid.len());
                count_cleared_replacements += cleared;
                overlay_changed = overlay_changed || cleared > 0;
            }
            ResolvedOp::ClearReplaceAll => {
                let before = state.context_overlay_snapshot();
                state.clear_context_replacements();
                let after = state.context_overlay_snapshot();
                let cleared = before
                    .replacements_by_rid
                    .len()
                    .saturating_sub(after.replacements_by_rid.len());
                count_cleared_replacements += cleared;
                overlay_changed = overlay_changed || cleared > 0;
            }
            ResolvedOp::AddNote { notes } => {
                let before_len = state.context_overlay_snapshot().notes.len();
                state.add_context_notes(notes.clone());
                let after_len = state.context_overlay_snapshot().notes.len();
                let added = after_len.saturating_sub(before_len);
                count_notes_added += added;
                overlay_changed = overlay_changed || added > 0;
            }
            ResolvedOp::RemoveNote { note_indices } => {
                let before_len = state.context_overlay_snapshot().notes.len();
                state.remove_context_notes(note_indices);
                let after_len = state.context_overlay_snapshot().notes.len();
                let removed = before_len.saturating_sub(after_len);
                count_notes_removed += removed;
                overlay_changed = overlay_changed || removed > 0;
            }
            ResolvedOp::ClearNotes => {
                let before_len = state.context_overlay_snapshot().notes.len();
                state.clear_context_notes();
                let after_len = state.context_overlay_snapshot().notes.len();
                let removed = before_len.saturating_sub(after_len);
                if removed > 0 {
                    cleared_notes = true;
                }
                count_notes_removed += removed;
                overlay_changed = overlay_changed || removed > 0;
            }
        }
    }

    let affected_call_ids =
        summarize_strings_for_json(call_ids_for_rids(&call_id_by_rid, &affected_rids));
    let missing_call_ids =
        summarize_strings_for_json(call_ids_for_rids(&call_id_by_rid, &missing_rids));
    let affected_ids = summarize_rids_for_json(affected_rids);
    let missing_ids = summarize_rids_for_json(missing_rids);

    let summary = json!({
        "included": count_included,
        "excluded": count_excluded,
        "deleted": count_deleted,
        "replaced": count_replaced,
        "cleared_replacements": count_cleared_replacements,
        "notes_added": count_notes_added,
        "notes_removed": count_notes_removed,
        "notes_cleared": cleared_notes,
        "skipped_missing_targets": skipped_missing_targets,
        "affected_ids": affected_ids,
        "missing_ids": missing_ids,
        "affected_call_ids": affected_call_ids,
        "missing_call_ids": missing_call_ids,
    });

    Ok((summary, include_changed, overlay_changed, deleted_rids))
}

fn summarize_rids_for_json(mut rids: Vec<u64>) -> serde_json::Value {
    const MAX: usize = 50;

    rids.sort_unstable();
    rids.dedup();

    let truncated = rids.len() > MAX;
    let ids = rids
        .into_iter()
        .take(MAX)
        .map(rid_to_string)
        .collect::<Vec<_>>();
    json!({
        "ids": ids,
        "truncated": truncated,
    })
}

fn summarize_strings_for_json(mut values: Vec<String>) -> serde_json::Value {
    const MAX: usize = 50;

    values.sort();
    values.dedup();

    let truncated = values.len() > MAX;
    values.truncate(MAX);

    json!({
        "ids": values,
        "truncated": truncated,
    })
}

fn call_id_by_rid(items: &[ResponseItem], rids: &[u64]) -> HashMap<u64, String> {
    let mut out: HashMap<u64, String> = HashMap::new();
    for (item, rid) in items.iter().zip(rids.iter().copied()) {
        let Some(call_id) = call_id_for_item(item) else {
            continue;
        };
        out.insert(rid, call_id.to_string());
    }
    out
}

fn call_ids_for_rids(call_id_by_rid: &HashMap<u64, String>, rids: &[u64]) -> Vec<String> {
    rids.iter()
        .filter_map(|rid| call_id_by_rid.get(rid))
        .cloned()
        .collect()
}

fn indices_for_rids(
    state: &crate::state::SessionState,
    rids: &[u64],
    skipped_missing_targets: &mut usize,
    missing_rids: &mut Vec<u64>,
) -> Vec<usize> {
    if rids.is_empty() {
        return Vec::new();
    }

    let snapshot_rids = state.history_rids_snapshot_lenient();
    let mut rid_lookup: HashMap<u64, usize> = HashMap::new();
    for (idx, rid) in snapshot_rids.into_iter().enumerate() {
        rid_lookup.insert(rid, idx);
    }

    let mut out = Vec::with_capacity(rids.len());
    for rid in rids {
        if let Some(idx) = rid_lookup.get(rid).copied() {
            out.push(idx);
        } else {
            *skipped_missing_targets += 1;
            missing_rids.push(*rid);
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

fn call_id_for_item(item: &ResponseItem) -> Option<&str> {
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
