use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::protocol::REASONING_CONTEXT_CLOSE_TAG;
use crate::protocol::REASONING_CONTEXT_OPEN_TAG;
use crate::protocol::TOOL_CONTEXT_CLOSE_TAG;
use crate::protocol::TOOL_CONTEXT_OPEN_TAG;
use crate::rid::parse_rid;
use crate::state::ContextItemsEvent;
use crate::state::ContextOverlay;
use crate::state::PruneCategory;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha1::Digest;
use std::collections::HashMap;
use std::collections::HashSet;

pub struct ManageContextHandler;

impl ManageContextHandler {
    pub(crate) fn strip_completed_manage_context_pairs_from_prompt_snapshot(
        current_history: &[ResponseItem],
        prompt_snapshot: &mut Vec<ResponseItem>,
    ) {
        let (in_flight_function_call_ids, in_flight_custom_call_ids) =
            in_flight_manage_context_call_ids(current_history);
        strip_completed_manage_context_pairs(
            prompt_snapshot,
            &in_flight_function_call_ids,
            &in_flight_custom_call_ids,
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageContextToolArgs {
    mode: String,
    #[serde(default)]
    policy_id: Option<String>,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    state_hash: Option<String>,
    #[serde(default)]
    chunk_summaries: Option<Vec<ChunkSummaryInput>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkSummaryInput {
    chunk_id: String,
    tool_context: String,
    reasoning_context: String,
}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    TargetReached,
    FixedPointReached,
    InvalidSummarySchema,
    InvalidContract,
    StateHashMismatch,
    PlanIdInvalid,
    RolloutPersistError,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TargetReached => "target_reached",
            Self::FixedPointReached => "fixed_point_reached",
            Self::InvalidSummarySchema => "invalid_summary_schema",
            Self::InvalidContract => "invalid_contract",
            Self::StateHashMismatch => "state_hash_mismatch",
            Self::PlanIdInvalid => "plan_id_invalid",
            Self::RolloutPersistError => "rollout_persist_error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct TopOffender {
    index: usize,
    id: Option<String>,
    category: String,
    approx_bytes: u64,
    preview: String,
    call_id: Option<String>,
    tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ChunkManifestEntry {
    chunk_id: String,
    source_id: Option<String>,
    index: usize,
    category: String,
    call_id: Option<String>,
    approx_bytes: u64,
    preview: String,
}

#[derive(Debug, Clone)]
struct RetrievePlan {
    state_hash: String,
    plan_id: String,
    chunk_manifest: Vec<ChunkManifestEntry>,
    top_offenders: Vec<TopOffender>,
}

#[derive(Debug)]
struct ManageContextResult {
    json: serde_json::Value,
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
            return Err(contract_error(
                StopReason::InvalidContract,
                "manage_context handler received unsupported payload",
            ));
        };

        let args: ManageContextToolArgs = serde_json::from_str(&arguments).map_err(|e| {
            contract_error(
                StopReason::InvalidContract,
                format!("failed to parse function arguments: {e}"),
            )
        })?;

        let result = handle_manage_context(session.as_ref(), turn.as_ref(), &args).await?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(
                serde_json::to_string(&result.json).unwrap_or_else(|_| "{}".to_string()),
            ),
            success: Some(true),
        })
    }
}

async fn handle_manage_context(
    session: &Session,
    turn: &TurnContext,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    match args.mode.as_str() {
        "retrieve" => handle_retrieve(session, turn, args).await,
        "apply" => handle_apply(session, turn, args).await,
        other => Err(contract_error(
            StopReason::InvalidContract,
            format!("manage_context.mode must be 'retrieve' or 'apply' (got '{other}')"),
        )),
    }
}

async fn handle_retrieve(
    session: &Session,
    turn: &TurnContext,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    // Merge-safety anchor: `/sanitize` and `~/.codex/manage_context*.md` depend on
    // retrieve rejecting apply-only fields instead of silently ignoring them.
    if args.plan_id.is_some() || args.state_hash.is_some() || args.chunk_summaries.is_some() {
        return Err(contract_error(
            StopReason::InvalidContract,
            "manage_context.retrieve accepts only mode and policy_id",
        ));
    }

    let policy_id = required_non_empty_str("policy_id", args.policy_id.as_ref())?;
    validate_policy_id(&policy_id, turn)?;

    let max_chunks = turn.config.manage_context_policy.max_chunks_per_apply;
    let plan = {
        let state = session.state.lock().await;
        build_retrieve_plan(&state, &policy_id)
    };
    let remaining_apply_batches = if max_chunks == 0 {
        0
    } else {
        plan.chunk_manifest.len().div_ceil(max_chunks)
    };

    Ok(ManageContextResult {
        json: json!({
            "mode": "retrieve",
            "plan_id": plan.plan_id,
            "state_hash": plan.state_hash,
            "policy_id": policy_id,
            "chunk_manifest": plan.chunk_manifest,
            "convergence_policy": {
                "fixed_point_k": turn.config.manage_context_policy.fixed_point_k,
                "stalled_signature_threshold": turn.config.manage_context_policy.stalled_signature_threshold,
                "max_chunks_per_apply": turn.config.manage_context_policy.max_chunks_per_apply,
                "quality_rubric_id": turn.config.manage_context_policy.quality_rubric_id,
            },
            "top_offenders": plan.top_offenders,
            "progress_report": {
                "manifest_chunk_count": plan.chunk_manifest.len(),
                "remaining_apply_batches": remaining_apply_batches,
                "max_chunks_per_apply": max_chunks,
            }
        }),
    })
}

async fn handle_apply(
    session: &Session,
    turn: &TurnContext,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    let policy_id = required_non_empty_str("policy_id", args.policy_id.as_ref())?;
    validate_policy_id(&policy_id, turn)?;

    let plan_id = required_non_empty_str("plan_id", args.plan_id.as_ref())?;
    let expected_state_hash = required_non_empty_str("state_hash", args.state_hash.as_ref())?;
    let chunk_summaries = args.chunk_summaries.clone().ok_or_else(|| {
        contract_error(
            StopReason::InvalidContract,
            "manage_context.apply requires chunk_summaries",
        )
    })?;

    if chunk_summaries.is_empty() {
        return Err(contract_error(
            StopReason::InvalidContract,
            "manage_context.apply requires non-empty chunk_summaries",
        ));
    }

    let max_chunks_per_apply = turn.config.manage_context_policy.max_chunks_per_apply;
    if chunk_summaries.len() > max_chunks_per_apply {
        return Err(contract_error(
            StopReason::InvalidContract,
            format!(
                "manage_context.apply chunk_summaries exceeds max_chunks_per_apply ({max_chunks_per_apply})"
            ),
        ));
    }

    let recorder = {
        let guard = session.services.rollout.lock().await;
        guard.clone()
    };

    let (
        applied_events,
        new_state_hash,
        progress_report,
        stop_reason,
        replacement_history,
        checkpoint,
    ) = {
        let mut state = session.state.lock().await;
        let checkpoint = state.manage_context_checkpoint();

        let current_plan = build_retrieve_plan(&state, &policy_id);

        if expected_state_hash != current_plan.state_hash {
            return Err(contract_error(
                StopReason::StateHashMismatch,
                format!(
                    "state_hash mismatch (expected {}, got {})",
                    expected_state_hash, current_plan.state_hash
                ),
            ));
        }

        if plan_id != current_plan.plan_id {
            return Err(contract_error(
                StopReason::PlanIdInvalid,
                format!(
                    "plan_id mismatch (expected {}, got {})",
                    current_plan.plan_id, plan_id
                ),
            ));
        }

        validate_chunk_summaries(&chunk_summaries, &current_plan.chunk_manifest)?;
        let manifest_by_chunk_id: HashMap<&str, &ChunkManifestEntry> = current_plan
            .chunk_manifest
            .iter()
            .map(|entry| (entry.chunk_id.as_str(), entry))
            .collect();

        let mut generated_notes = Vec::with_capacity(chunk_summaries.len() * 2);
        let mut applied_events = Vec::with_capacity(chunk_summaries.len());
        let mut indices_to_exclude = Vec::with_capacity(chunk_summaries.len());
        let mut replacement_updates = Vec::with_capacity(chunk_summaries.len());

        for chunk in &chunk_summaries {
            let chunk_id = chunk.chunk_id.trim();
            let manifest_entry = manifest_by_chunk_id.get(chunk_id).ok_or_else(|| {
                contract_error(
                    StopReason::InvalidSummarySchema,
                    format!("chunk_id '{chunk_id}' is not present in current chunk_manifest"),
                )
            })?;

            let tool_context_note = build_tool_context_note(chunk, &plan_id);
            let reasoning_context_note = build_reasoning_context_note(chunk, &plan_id);

            generated_notes.push(tool_context_note.clone());
            generated_notes.push(reasoning_context_note.clone());

            let replacement_text = chunk_replacement_text(chunk, &plan_id);
            let replacement_rid = manifest_entry.source_id.as_deref().and_then(parse_rid);
            let replacement_applied = replacement_rid
                .filter(|_| replacement_text.len() as u64 <= manifest_entry.approx_bytes)
                .map(|rid| {
                    replacement_updates.push((rid, replacement_text));
                    true
                })
                .unwrap_or(false);
            let excluded = if replacement_applied {
                false
            } else {
                indices_to_exclude.push(manifest_entry.index);
                true
            };

            applied_events.push(json!({
                "chunk_id": chunk_id,
                "source_id": manifest_entry.source_id,
                "index": manifest_entry.index,
                "excluded": excluded,
                "replacement_applied": replacement_applied,
                "tool_context": tool_context_note,
                "reasoning_context": reasoning_context_note,
            }));
        }

        debug_assert_eq!(generated_notes.len(), chunk_summaries.len() * 2);

        indices_to_exclude.sort_unstable();
        indices_to_exclude.dedup();
        if !indices_to_exclude.is_empty() {
            state.set_context_inclusion(&indices_to_exclude, false);
        }
        let replaced_chunks = replacement_updates.len();
        if !replacement_updates.is_empty() {
            state.upsert_context_replacements(replacement_updates);
        }

        let notes_added = generated_notes.len();
        state.add_context_notes(generated_notes);
        // Merge-safety anchor: this snapshot is the source persisted into rollout
        // replacement_history and later consumed by resume/recall boundaries.
        let replacement_history = materialize_prompt_snapshot_after_apply(&mut state);

        let final_plan = build_retrieve_plan(&state, &policy_id);
        let new_state_hash = final_plan.state_hash;
        let remaining_manifest_chunks = final_plan.chunk_manifest.len();
        let stop_reason = if remaining_manifest_chunks == 0 {
            StopReason::FixedPointReached
        } else {
            StopReason::TargetReached
        };

        let progress_report = json!({
            "requested_chunks": chunk_summaries.len(),
            "applied_chunks": applied_events.len(),
            "excluded_chunks": indices_to_exclude.len(),
            "replaced_chunks": replaced_chunks,
            "notes_added": notes_added,
            "manifest_chunk_count_before": current_plan.chunk_manifest.len(),
            "remaining_manifest_chunks": remaining_manifest_chunks,
            "manifest_chunk_count_after": remaining_manifest_chunks,
            "max_chunks_per_apply": max_chunks_per_apply,
        });

        (
            applied_events,
            new_state_hash,
            progress_report,
            stop_reason,
            replacement_history,
            checkpoint,
        )
    };

    if let Some(replacement_history) = replacement_history
        && let Some(recorder) = recorder
    {
        let compacted_item = RolloutItem::Compacted(CompactedItem {
            message: String::new(),
            replacement_history: Some(replacement_history),
        });
        if let Err(error) = recorder.record_items(&[compacted_item]).await {
            let mut state = session.state.lock().await;
            state.restore_manage_context_checkpoint(checkpoint);
            return Err(contract_error(
                StopReason::RolloutPersistError,
                format!("failed to persist compacted replacement_history: {error}"),
            ));
        }
    }

    Ok(ManageContextResult {
        json: json!({
            "mode": "apply",
            "applied_events": applied_events,
            "new_state_hash": new_state_hash,
            "progress_report": progress_report,
            "stop_reason": stop_reason.as_str(),
        }),
    })
}

fn validate_policy_id(policy_id: &str, turn: &TurnContext) -> Result<(), FunctionCallError> {
    let expected = turn.config.manage_context_policy.quality_rubric_id.trim();
    if policy_id != expected {
        return Err(contract_error(
            StopReason::InvalidContract,
            format!("policy_id mismatch (expected '{expected}', got '{policy_id}')"),
        ));
    }
    Ok(())
}

fn required_non_empty_str(
    field_name: &str,
    value: Option<&String>,
) -> Result<String, FunctionCallError> {
    let value = value.ok_or_else(|| {
        contract_error(
            StopReason::InvalidContract,
            format!("manage_context.{field_name} is required"),
        )
    })?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(contract_error(
            StopReason::InvalidContract,
            format!("manage_context.{field_name} must be non-empty"),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_chunk_summaries(
    chunk_summaries: &[ChunkSummaryInput],
    chunk_manifest: &[ChunkManifestEntry],
) -> Result<(), FunctionCallError> {
    let manifest_ids: HashSet<&str> = chunk_manifest
        .iter()
        .map(|entry| entry.chunk_id.as_str())
        .collect();
    let mut seen_chunk_ids: HashSet<&str> = HashSet::new();

    for chunk in chunk_summaries {
        let chunk_id = chunk.chunk_id.trim();
        if chunk_id.is_empty() {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                "manage_context.apply chunk_summaries[].chunk_id must be non-empty",
            ));
        }
        if chunk.tool_context.trim().is_empty() {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                format!(
                    "manage_context.apply chunk_summaries[{chunk_id}].tool_context must be non-empty"
                ),
            ));
        }
        if chunk.reasoning_context.trim().is_empty() {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                format!(
                    "manage_context.apply chunk_summaries[{chunk_id}].reasoning_context must be non-empty"
                ),
            ));
        }
        if !manifest_ids.contains(chunk_id) {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                format!("chunk_id '{chunk_id}' is not present in current chunk_manifest"),
            ));
        }
        if !seen_chunk_ids.insert(chunk_id) {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                format!("chunk_id '{chunk_id}' appears more than once in apply payload"),
            ));
        }
    }

    Ok(())
}

fn chunk_replacement_text(chunk: &ChunkSummaryInput, plan_id: &str) -> String {
    format!(
        "chunk_id={}\nplan_id={}\n{}\n{}",
        chunk.chunk_id.trim(),
        plan_id,
        chunk.tool_context.trim(),
        chunk.reasoning_context.trim()
    )
}

fn build_tool_context_note(chunk: &ChunkSummaryInput, plan_id: &str) -> String {
    format!(
        "{TOOL_CONTEXT_OPEN_TAG}\nchunk_id={}\nplan_id={plan_id}\n{}\n{TOOL_CONTEXT_CLOSE_TAG}",
        chunk.chunk_id.trim(),
        chunk.tool_context.trim(),
    )
}

fn build_reasoning_context_note(chunk: &ChunkSummaryInput, plan_id: &str) -> String {
    format!(
        "{REASONING_CONTEXT_OPEN_TAG}\nchunk_id={}\nplan_id={plan_id}\n{}\n{REASONING_CONTEXT_CLOSE_TAG}",
        chunk.chunk_id.trim(),
        chunk.reasoning_context.trim(),
    )
}

fn build_retrieve_plan(state: &crate::state::SessionState, policy_id: &str) -> RetrievePlan {
    let ev = state.build_context_items_event();
    let overlay = state.context_overlay_snapshot();
    let items = state.history_snapshot_lenient();
    let last_user_index = latest_user_message_index(&items);

    let state_hash = state_hash_for_context(&ev, &overlay, &items);
    let top_offenders = collect_top_offenders(&ev.items, &items, &overlay, last_user_index);
    let chunk_manifest = top_offenders
        .iter()
        .enumerate()
        .map(|(idx, offender)| ChunkManifestEntry {
            chunk_id: format!("chunk_{:03}", idx + 1),
            source_id: offender.id.clone(),
            index: offender.index,
            category: offender.category.clone(),
            call_id: offender.call_id.clone(),
            approx_bytes: offender.approx_bytes,
            preview: offender.preview.clone(),
        })
        .collect::<Vec<_>>();
    let plan_id = plan_id_for(policy_id, &state_hash, &chunk_manifest);

    RetrievePlan {
        state_hash,
        plan_id,
        chunk_manifest,
        top_offenders,
    }
}

fn collect_top_offenders(
    summaries: &[crate::state::ContextItemSummary],
    items: &[ResponseItem],
    overlay: &ContextOverlay,
    last_user_index: Option<usize>,
) -> Vec<TopOffender> {
    let manage_context_call_ownership = ManageContextCallOwnership::from_items(items);
    let tool_name_by_call_id = tool_name_by_call_id(items);

    let mut top_offenders = Vec::new();

    for summary in summaries {
        // Post-user append-only items are intentionally excluded from retrieve
        // planning to keep retrieve/apply stable within the same turn.
        if last_user_index.is_some_and(|cutoff| summary.index > cutoff) {
            continue;
        }
        if !summary.included {
            continue;
        }
        if !matches!(
            summary.category,
            PruneCategory::ToolOutput | PruneCategory::Reasoning
        ) {
            continue;
        }

        let item = items.get(summary.index);
        if item
            .is_some_and(|raw_item| manage_context_call_ownership.is_manage_context_item(raw_item))
        {
            continue;
        }

        let raw_bytes = item.map(estimate_item_bytes).unwrap_or(0);
        let effective_bytes = summary
            .id
            .as_deref()
            .and_then(parse_rid)
            .and_then(|rid| overlay.replacements_by_rid.get(&rid))
            .map(|replacement| replacement.len() as u64)
            .unwrap_or(raw_bytes);

        let call_id = item.and_then(call_id_for_item).map(ToString::to_string);
        let tool_name = call_id
            .as_deref()
            .and_then(|call_id| tool_name_by_call_id.get(call_id))
            .map(|name| (*name).to_string());

        top_offenders.push(TopOffender {
            index: summary.index,
            id: summary.id.clone(),
            category: prune_category_tag(summary.category).to_string(),
            approx_bytes: effective_bytes,
            preview: summary.preview.clone(),
            call_id,
            tool_name,
        });
    }

    top_offenders.sort_by(|lhs, rhs| {
        rhs.approx_bytes
            .cmp(&lhs.approx_bytes)
            .then_with(|| lhs.index.cmp(&rhs.index))
    });
    top_offenders
}

fn materialize_prompt_snapshot_after_apply(
    state: &mut crate::state::SessionState,
) -> Option<Vec<ResponseItem>> {
    let current_history = state.history_snapshot_lenient();
    let mut prompt_snapshot = state.prompt_snapshot_lenient();
    ManageContextHandler::strip_completed_manage_context_pairs_from_prompt_snapshot(
        &current_history,
        &mut prompt_snapshot,
    );

    if prompt_snapshot == current_history {
        return None;
    }

    let reference_context_item = state.reference_context_item();
    state.replace_history(prompt_snapshot.clone(), reference_context_item);
    Some(prompt_snapshot)
}

fn in_flight_manage_context_call_ids(items: &[ResponseItem]) -> (HashSet<String>, HashSet<String>) {
    let mut manage_context_function_call_ids: HashSet<String> = HashSet::new();
    let mut manage_context_custom_call_ids: HashSet<String> = HashSet::new();
    let mut function_output_ids: HashSet<String> = HashSet::new();
    let mut custom_output_ids: HashSet<String> = HashSet::new();

    for item in items {
        match item {
            ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                manage_context_function_call_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                manage_context_custom_call_ids.insert(call_id.clone());
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                function_output_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                custom_output_ids.insert(call_id.clone());
            }
            _ => {}
        }
    }

    let in_flight_function_call_ids: HashSet<String> = manage_context_function_call_ids
        .into_iter()
        .filter(|call_id| !function_output_ids.contains(call_id))
        .collect();
    let in_flight_custom_call_ids: HashSet<String> = manage_context_custom_call_ids
        .into_iter()
        .filter(|call_id| !custom_output_ids.contains(call_id))
        .collect();

    (in_flight_function_call_ids, in_flight_custom_call_ids)
}

fn strip_completed_manage_context_pairs(
    items: &mut Vec<ResponseItem>,
    in_flight_function_call_ids: &HashSet<String>,
    in_flight_custom_call_ids: &HashSet<String>,
) {
    let mut manage_context_function_call_ids: HashSet<String> = HashSet::new();
    let mut manage_context_custom_call_ids: HashSet<String> = HashSet::new();
    let mut function_output_ids: HashSet<String> = HashSet::new();
    let mut custom_output_ids: HashSet<String> = HashSet::new();
    let mut non_manage_function_like_call_ids: HashSet<String> = HashSet::new();
    let mut non_manage_custom_call_ids: HashSet<String> = HashSet::new();

    for item in items.iter() {
        match item {
            ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                manage_context_function_call_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                manage_context_custom_call_ids.insert(call_id.clone());
            }
            ResponseItem::FunctionCall { call_id, .. } => {
                non_manage_function_like_call_ids.insert(call_id.clone());
            }
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                non_manage_function_like_call_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                non_manage_custom_call_ids.insert(call_id.clone());
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                if !in_flight_function_call_ids.contains(call_id) {
                    function_output_ids.insert(call_id.clone());
                }
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                if !in_flight_custom_call_ids.contains(call_id) {
                    custom_output_ids.insert(call_id.clone());
                }
            }
            _ => {}
        }
    }

    let completed_function_call_ids: HashSet<String> = manage_context_function_call_ids
        .into_iter()
        .filter(|call_id| function_output_ids.contains(call_id))
        .collect();
    let completed_custom_call_ids: HashSet<String> = manage_context_custom_call_ids
        .into_iter()
        .filter(|call_id| custom_output_ids.contains(call_id))
        .collect();

    if completed_function_call_ids.is_empty()
        && completed_custom_call_ids.is_empty()
        && in_flight_function_call_ids.is_empty()
        && in_flight_custom_call_ids.is_empty()
    {
        return;
    }

    items.retain(|item| match item {
        ResponseItem::FunctionCall { name, call_id, .. } => {
            !(name == "manage_context" && completed_function_call_ids.contains(call_id))
        }
        ResponseItem::CustomToolCall { name, call_id, .. } => {
            !(name == "manage_context" && completed_custom_call_ids.contains(call_id))
        }
        ResponseItem::FunctionCallOutput { call_id, .. } => {
            if in_flight_function_call_ids.contains(call_id) {
                return false;
            }
            !completed_function_call_ids.contains(call_id)
                || non_manage_function_like_call_ids.contains(call_id)
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => {
            if in_flight_custom_call_ids.contains(call_id) {
                return false;
            }
            !completed_custom_call_ids.contains(call_id)
                || non_manage_custom_call_ids.contains(call_id)
        }
        _ => true,
    });
}

#[derive(Default)]
struct ManageContextCallOwnership {
    manage_context_function_call_ids: HashSet<String>,
    manage_context_custom_call_ids: HashSet<String>,
    non_manage_function_like_call_ids: HashSet<String>,
    non_manage_custom_call_ids: HashSet<String>,
}

impl ManageContextCallOwnership {
    fn from_items(items: &[ResponseItem]) -> Self {
        let mut call_ownership = Self::default();
        for item in items {
            match item {
                ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                    call_ownership
                        .manage_context_function_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                    call_ownership
                        .manage_context_custom_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::FunctionCall { call_id, .. } => {
                    call_ownership
                        .non_manage_function_like_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => {
                    call_ownership
                        .non_manage_function_like_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::CustomToolCall { call_id, .. } => {
                    call_ownership
                        .non_manage_custom_call_ids
                        .insert(call_id.clone());
                }
                _ => {}
            }
        }
        call_ownership
    }

    fn is_manage_context_item(&self, item: &ResponseItem) -> bool {
        match item {
            ResponseItem::FunctionCall { name, .. } | ResponseItem::CustomToolCall { name, .. } => {
                name == "manage_context"
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                self.manage_context_function_call_ids.contains(call_id)
                    && !self.non_manage_function_like_call_ids.contains(call_id)
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                self.manage_context_custom_call_ids.contains(call_id)
                    && !self.non_manage_custom_call_ids.contains(call_id)
            }
            _ => false,
        }
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

fn latest_user_message_index(items: &[ResponseItem]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| match item {
            ResponseItem::Message { role, .. } if role == "user" => Some(index),
            _ => None,
        })
}

fn estimate_item_bytes(item: &ResponseItem) -> u64 {
    serde_json::to_vec(item)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
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

fn state_hash_for_context(
    ev: &ContextItemsEvent,
    overlay: &ContextOverlay,
    items: &[ResponseItem],
) -> String {
    let last_user_index = latest_user_message_index(items);
    let manage_context_call_ownership = ManageContextCallOwnership::from_items(items);

    let mut hasher = sha1::Sha1::new();
    hasher.update(b"items\n");
    for item in &ev.items {
        // Ignore append-only items after the latest user message so retrieve/apply
        // remains stable within a single sanitize turn.
        if last_user_index.is_some_and(|cutoff| item.index > cutoff) {
            continue;
        }
        if let Some(raw_item) = items.get(item.index)
            && manage_context_call_ownership.is_manage_context_item(raw_item)
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

fn plan_id_for(policy_id: &str, state_hash: &str, chunk_manifest: &[ChunkManifestEntry]) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(b"policy\n");
    hasher.update(policy_id.as_bytes());
    hasher.update(b"\nstate_hash\n");
    hasher.update(state_hash.as_bytes());
    hasher.update(b"\nchunks\n");

    for chunk in chunk_manifest {
        hasher.update(chunk.chunk_id.as_bytes());
        hasher.update(b"\n");
        if let Some(source_id) = &chunk.source_id {
            hasher.update(source_id.as_bytes());
        }
        hasher.update(b"\n");
        hasher.update((chunk.index as u64).to_le_bytes());
        hasher.update(b"\n");
        hasher.update(chunk.category.as_bytes());
        hasher.update(b"\n");
        if let Some(call_id) = &chunk.call_id {
            hasher.update(call_id.as_bytes());
        }
        hasher.update(b"\n");
        hasher.update(chunk.approx_bytes.to_le_bytes());
        hasher.update(b"\n");
    }

    format!("{:x}", hasher.finalize())
}

fn contract_error(reason: StopReason, message: impl Into<String>) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        json!({
            "stop_reason": reason.as_str(),
            "message": message.into(),
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ModelClient;
    use crate::features::Feature;
    use crate::protocol::Event;
    use crate::protocol::EventMsg;
    use crate::tasks::RegularTask;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::LocalShellAction;
    use codex_protocol::models::LocalShellExecAction;
    use codex_protocol::models::LocalShellStatus;
    use codex_protocol::openai_models::InputModality;
    use codex_protocol::user_input::UserInput;
    use serde_json::Value;
    use std::fmt::Write as _;
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    fn user_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn assistant_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn tool_call(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "exec_command".to_string(),
            arguments: r#"{"cmd":"echo test"}"#.to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn local_shell_call(call_id: &str) -> ResponseItem {
        ResponseItem::LocalShellCall {
            id: None,
            call_id: Some(call_id.to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["echo".to_string(), "test".to_string()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
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

    fn tool_output(call_id: &str, text: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(text.to_string()),
                success: Some(true),
            },
        }
    }

    fn manage_context_output(call_id: &str, text: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(text.to_string()),
                success: Some(true),
            },
        }
    }

    fn custom_tool_call(name: &str, call_id: &str) -> ResponseItem {
        ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: call_id.to_string(),
            name: name.to_string(),
            input: "{}".to_string(),
        }
    }

    fn custom_tool_output(call_id: &str, output: &str) -> ResponseItem {
        ResponseItem::CustomToolCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload::from_text(
                output.to_string(),
            ),
        }
    }

    fn parse_error_stop_reason(err: FunctionCallError) -> String {
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected RespondToModel error");
        };
        let parsed: Value =
            serde_json::from_str(&message).expect("structured manage_context error");
        parsed
            .get("stop_reason")
            .and_then(Value::as_str)
            .expect("stop_reason")
            .to_string()
    }

    fn response_created(id: &str) -> Value {
        json!({
            "type": "response.created",
            "response": {
                "id": id,
            }
        })
    }

    fn assistant_message_event(id: &str, text: &str) -> Value {
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "id": id,
                "content": [{"type": "output_text", "text": text}]
            }
        })
    }

    fn response_completed(id: &str) -> Value {
        json!({
            "type": "response.completed",
            "response": {
                "id": id,
                "usage": {
                    "input_tokens": 0,
                    "input_tokens_details": null,
                    "output_tokens": 0,
                    "output_tokens_details": null,
                    "total_tokens": 0
                }
            }
        })
    }

    fn sse(events: &[Value]) -> String {
        let mut output = String::new();
        for event in events {
            let event_type = event
                .get("type")
                .and_then(Value::as_str)
                .expect("response event type");
            writeln!(&mut output, "event: {event_type}").expect("write SSE event type");
            write!(&mut output, "data: {event}\n\n").expect("write SSE event body");
        }
        output
    }

    fn decode_request_body(request: &wiremock::Request) -> Vec<u8> {
        let encoding = request
            .headers
            .get("content-encoding")
            .and_then(|value| value.to_str().ok());
        if encoding.is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        }) {
            zstd::stream::decode_all(Cursor::new(request.body.as_slice()))
                .expect("decode zstd request body")
        } else {
            request.body.clone()
        }
    }

    fn request_contains_manage_context_pair(body: &Value, call_id: &str) -> bool {
        body.get("input")
            .and_then(Value::as_array)
            .is_some_and(|input| {
                input
                    .iter()
                    .any(|item| match item.get("type").and_then(Value::as_str) {
                        Some("function_call") => {
                            item.get("name").and_then(Value::as_str) == Some("manage_context")
                        }
                        Some("function_call_output") => {
                            item.get("call_id").and_then(Value::as_str) == Some(call_id)
                        }
                        Some("custom_tool_call") => {
                            item.get("name").and_then(Value::as_str) == Some("manage_context")
                        }
                        Some("custom_tool_call_output") => {
                            item.get("call_id").and_then(Value::as_str) == Some(call_id)
                        }
                        _ => false,
                    })
            })
    }

    async fn wait_for_turn_complete(rx: &async_channel::Receiver<Event>) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = rx.recv().await.expect("event channel open");
                if matches!(event.msg, EventMsg::TurnComplete(_)) {
                    break;
                }
            }
        })
        .await
        .expect("timeout waiting for turn completion");
    }

    struct CaptureResponsesRequestResponder {
        calls: AtomicUsize,
        requests: Arc<Mutex<Vec<Value>>>,
        response_body: String,
    }

    impl Respond for CaptureResponsesRequestResponder {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let call_num = self.calls.fetch_add(1, Ordering::SeqCst);
            if call_num > 0 {
                panic!("unexpected extra model request {call_num}");
            }

            let parsed: Value = serde_json::from_slice(&decode_request_body(request))
                .expect("valid model request body");
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(parsed);

            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(self.response_body.clone())
        }
    }

    #[test]
    fn manage_context_stop_reason_contract_matches_v2_set() {
        let actual = [
            StopReason::TargetReached.as_str(),
            StopReason::FixedPointReached.as_str(),
            StopReason::InvalidSummarySchema.as_str(),
            StopReason::StateHashMismatch.as_str(),
            StopReason::PlanIdInvalid.as_str(),
            StopReason::InvalidContract.as_str(),
            StopReason::RolloutPersistError.as_str(),
        ];
        let expected = [
            "target_reached",
            "fixed_point_reached",
            "invalid_summary_schema",
            "state_hash_mismatch",
            "plan_id_invalid",
            "invalid_contract",
            "rollout_persist_error",
        ];
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn manage_context_retrieve_requires_policy_id() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let args: ManageContextToolArgs =
            serde_json::from_str(r#"{"mode":"retrieve"}"#).expect("parse args");

        let err = handle_manage_context(&session, &turn, &args)
            .await
            .expect_err("must reject missing policy_id");

        assert_eq!(
            parse_error_stop_reason(err),
            StopReason::InvalidContract.as_str()
        );
    }

    #[test]
    fn manage_context_contract_rejects_unknown_fields() {
        for payload in [
            r#"{"mode":"retrieve","policy_id":"p","unexpected_field":"x"}"#,
            r#"{"mode":"apply","policy_id":"p","plan_id":"x","state_hash":"h","unexpected_field":[]}"#,
        ] {
            let parsed: Result<ManageContextToolArgs, _> = serde_json::from_str(payload);
            assert!(
                parsed.is_err(),
                "unknown fields must fail strict contract parsing"
            );
        }
    }

    #[tokio::test]
    async fn manage_context_retrieve_rejects_apply_only_fields() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": "invalid-plan",
        }))
        .expect("parse args");

        let err = handle_manage_context(&session, &turn, &args)
            .await
            .expect_err("must reject apply-only fields in retrieve mode");

        assert_eq!(
            parse_error_stop_reason(err),
            StopReason::InvalidContract.as_str()
        );
    }

    #[tokio::test]
    async fn manage_context_retrieve_returns_required_fields() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("recent user ask"),
                    &assistant_message("recent assistant reply"),
                ],
                turn.truncation_policy,
            );
        }

        let args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse args");

        let result = handle_manage_context(&session, &turn, &args)
            .await
            .expect("retrieve must succeed");

        assert_eq!(
            result.json.get("mode").and_then(Value::as_str),
            Some("retrieve")
        );
        assert!(result.json.get("plan_id").is_some_and(Value::is_string));
        assert!(result.json.get("state_hash").is_some_and(Value::is_string));
        assert!(result.json.get("policy_id").is_some_and(Value::is_string));
        assert!(
            result
                .json
                .get("chunk_manifest")
                .is_some_and(Value::is_array)
        );
        assert!(
            result
                .json
                .get("convergence_policy")
                .is_some_and(Value::is_object)
        );
        assert!(
            result
                .json
                .get("top_offenders")
                .is_some_and(Value::is_array)
        );
    }

    #[tokio::test]
    async fn manage_context_retrieve_is_stable_when_items_append_after_latest_user_message() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &tool_call("call_pre_user"),
                    &tool_output("call_pre_user", "pre-user tool output"),
                    &user_message("latest user"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let first = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("first retrieve");
        assert!(
            first
                .json
                .get("chunk_manifest")
                .and_then(Value::as_array)
                .is_some_and(|chunks| !chunks.is_empty()),
            "fixture must produce non-empty chunk_manifest"
        );

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &assistant_message("appended after latest user"),
                    &tool_call("call_post_user"),
                    &tool_output("call_post_user", "post-user tool output"),
                ],
                turn.truncation_policy,
            );
        }

        let second = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("second retrieve");

        assert_eq!(
            first.json.get("state_hash"),
            second.json.get("state_hash"),
            "state_hash must stay stable for append-only post-user changes"
        );
        assert_eq!(
            first.json.get("plan_id"),
            second.json.get("plan_id"),
            "plan_id must stay stable for append-only post-user changes"
        );
        assert_eq!(
            first.json.get("chunk_manifest"),
            second.json.get("chunk_manifest"),
            "chunk_manifest must stay stable for append-only post-user changes"
        );
    }

    #[tokio::test]
    async fn manage_context_retrieve_excludes_post_user_offenders_from_chunk_manifest() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("latest user"),
                    &tool_call("call_post_user"),
                    &tool_output("call_post_user", "post-user tool output"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        assert_eq!(
            retrieve
                .json
                .get("chunk_manifest")
                .and_then(Value::as_array)
                .map(std::vec::Vec::len),
            Some(0),
            "post-user offenders are intentionally excluded from current retrieve plan"
        );
    }

    #[tokio::test]
    async fn manage_context_retrieve_never_targets_user_assistant_or_protected_categories() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message(
                        "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nkeep\n</INSTRUCTIONS>",
                    ),
                    &user_message("<environment_context>\ncwd: /repo\n</environment_context>"),
                    &assistant_message("assistant chatter"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload"),
                    &user_message("latest user"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");
        let categories = retrieve
            .json
            .get("chunk_manifest")
            .and_then(Value::as_array)
            .expect("chunk_manifest array")
            .iter()
            .filter_map(|entry| entry.get("category").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert!(
            !categories.is_empty(),
            "fixture must produce at least one targetable chunk"
        );
        assert!(
            categories
                .iter()
                .all(|category| { matches!(category, &"tool_output" | &"reasoning") })
        );
        assert!(!categories.iter().any(|category| {
            matches!(
                category,
                &"user_message"
                    | &"assistant_message"
                    | &"user_instructions"
                    | &"environment_context"
            )
        }));
    }

    #[test]
    fn strip_completed_manage_context_pairs_removes_call_and_output_together() {
        let mut items = vec![
            user_message("u1"),
            manage_context_call("done"),
            manage_context_output("done", "{\"mode\":\"retrieve\"}"),
            manage_context_call("pending"),
            tool_call("exec-1"),
            tool_output("exec-1", "ok"),
        ];

        strip_completed_manage_context_pairs(&mut items, &HashSet::new(), &HashSet::new());

        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "done"
            )
        }));
        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "done"
            )
        }));
        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "pending"
            )
        }));
        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "exec-1"
            )
        }));
    }

    #[test]
    fn strip_completed_manage_context_pairs_keeps_outputs_on_call_id_collision() {
        let mut items = vec![
            user_message("u1"),
            manage_context_call("shared"),
            manage_context_output("shared", "{\"mode\":\"retrieve\"}"),
            tool_call("shared"),
        ];

        strip_completed_manage_context_pairs(&mut items, &HashSet::new(), &HashSet::new());

        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "shared"
            )
        }));
        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "exec_command" && call_id == "shared"
            )
        }));
        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "shared"
            )
        }));
    }

    #[test]
    fn manage_context_call_ownership_keeps_function_output_on_call_id_collision() {
        let output = tool_output("shared", "ok");
        let items = vec![
            user_message("u1"),
            manage_context_call("shared"),
            manage_context_output("shared", "{\"mode\":\"retrieve\"}"),
            tool_call("shared"),
            output.clone(),
        ];

        let call_ownership = ManageContextCallOwnership::from_items(&items);

        assert!(!call_ownership.is_manage_context_item(&output));
    }

    #[test]
    fn manage_context_call_ownership_keeps_local_shell_output_on_call_id_collision() {
        let output = tool_output("shared", "ok");
        let items = vec![
            user_message("u1"),
            manage_context_call("shared"),
            manage_context_output("shared", "{\"mode\":\"retrieve\"}"),
            local_shell_call("shared"),
            output.clone(),
        ];

        let call_ownership = ManageContextCallOwnership::from_items(&items);

        assert!(!call_ownership.is_manage_context_item(&output));
    }

    #[test]
    fn strip_completed_manage_context_pairs_drops_function_output_when_only_custom_call_collides() {
        let mut items = vec![
            user_message("u1"),
            manage_context_call("shared"),
            manage_context_output("shared", "{\"mode\":\"retrieve\"}"),
            custom_tool_call("apply_patch", "shared"),
            custom_tool_output("shared", "ok"),
        ];

        strip_completed_manage_context_pairs(&mut items, &HashSet::new(), &HashSet::new());

        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "shared"
            )
        }));
        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::CustomToolCall { name, call_id, .. }
                    if name == "apply_patch" && call_id == "shared"
            )
        }));
        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "shared"
            )
        }));
        let mut manager = crate::context_manager::ContextManager::new();
        manager.replace(items);
        let _ = manager.for_prompt(&[InputModality::Text]);
    }

    #[test]
    fn strip_completed_manage_context_pairs_drops_custom_output_when_only_function_call_collides() {
        let mut items = vec![
            user_message("u1"),
            custom_tool_call("manage_context", "shared"),
            custom_tool_output("shared", "{\"mode\":\"retrieve\"}"),
            tool_call("shared"),
            tool_output("shared", "ok"),
        ];

        strip_completed_manage_context_pairs(&mut items, &HashSet::new(), &HashSet::new());

        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::CustomToolCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "shared"
            )
        }));
        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "exec_command" && call_id == "shared"
            )
        }));
        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::CustomToolCallOutput { call_id, .. } if call_id == "shared"
            )
        }));
        let mut manager = crate::context_manager::ContextManager::new();
        manager.replace(items);
        let _ = manager.for_prompt(&[InputModality::Text]);
    }

    #[test]
    fn strip_completed_manage_context_pairs_keeps_in_flight_call_and_drops_synthetic_output() {
        let mut items = vec![
            user_message("u1"),
            manage_context_call("done"),
            manage_context_output("done", "{\"mode\":\"retrieve\"}"),
            manage_context_call("in-flight"),
            manage_context_output("in-flight", "[codex] tool output omitted"),
        ];

        strip_completed_manage_context_pairs(
            &mut items,
            &HashSet::from(["in-flight".to_string()]),
            &HashSet::new(),
        );

        assert!(items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "in-flight"
            )
        }));
        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "in-flight"
            )
        }));
        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "done"
            )
        }));
        assert!(!items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "done"
            )
        }));

        let mut manager = crate::context_manager::ContextManager::new();
        manager.replace(items);
        let _ = manager.for_prompt(&[InputModality::Text]);
    }

    #[test]
    fn in_flight_manage_context_call_ids_detects_unfinished_pairs() {
        let items = vec![
            manage_context_call("done-fn"),
            manage_context_output("done-fn", "{}"),
            manage_context_call("pending-fn"),
            custom_tool_call("manage_context", "done-custom"),
            custom_tool_output("done-custom", "{}"),
            custom_tool_call("manage_context", "pending-custom"),
        ];

        let (in_flight_function_ids, in_flight_custom_ids) =
            in_flight_manage_context_call_ids(&items);

        assert_eq!(
            in_flight_function_ids,
            HashSet::from(["pending-fn".to_string()])
        );
        assert_eq!(
            in_flight_custom_ids,
            HashSet::from(["pending-custom".to_string()])
        );
    }

    #[tokio::test]
    async fn materialize_prompt_snapshot_after_apply_keeps_in_flight_manage_context_call() {
        let (session, turn) = crate::codex::make_session_and_context().await;

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("seed user"),
                    &manage_context_call("in-flight"),
                    &tool_call("exec-1"),
                    &tool_output("exec-1", "tool output"),
                ],
                turn.truncation_policy,
            );

            // Force prompt_snapshot_lenient to run lenient placeholder injection logic.
            state.set_context_inclusion(&[0], false);

            let _ = materialize_prompt_snapshot_after_apply(&mut state);

            let history_after_materialize = state.history_snapshot_lenient();
            assert!(history_after_materialize.iter().any(|item| {
                matches!(
                    item,
                    ResponseItem::FunctionCall { name, call_id, .. }
                        if name == "manage_context" && call_id == "in-flight"
                )
            }));
            assert!(!history_after_materialize.iter().any(|item| {
                matches!(
                    item,
                    ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "in-flight"
                )
            }));

            state.record_items(
                [&manage_context_output("in-flight", "{\"mode\":\"apply\"}")],
                turn.truncation_policy,
            );
            let final_history = state.history_snapshot_lenient();
            let mut manager = crate::context_manager::ContextManager::new();
            manager.replace(final_history);
            let _ = manager.for_prompt(&[InputModality::Text]);
        }
    }

    #[tokio::test]
    async fn manage_context_apply_accepts_append_only_post_user_changes() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &tool_call("call_base"),
                    &tool_output("call_base", "base tool output"),
                    &user_message("latest user"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();
        let chunk_id = retrieve
            .json
            .pointer("/chunk_manifest/0/chunk_id")
            .and_then(Value::as_str)
            .expect("first chunk id")
            .to_string();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &assistant_message("appended assistant"),
                    &tool_call("call_post_user"),
                    &tool_output("call_post_user", "post-user output"),
                ],
                turn.truncation_policy,
            );
        }

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": state_hash,
            "chunk_summaries": [{
                "chunk_id": chunk_id,
                "tool_context": "tool summary",
                "reasoning_context": "reasoning summary"
            }]
        }))
        .expect("parse apply args");

        let result = handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect("apply should accept append-only post-user changes");

        assert_eq!(
            result.json.get("mode").and_then(Value::as_str),
            Some("apply")
        );
    }

    #[tokio::test]
    async fn manage_context_apply_requires_chunk_summaries() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        let args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": policy_id,
            "plan_id": "p",
            "state_hash": "h",
        }))
        .expect("parse args");

        let err = handle_manage_context(&session, &turn, &args)
            .await
            .expect_err("must reject missing chunk_summaries");

        assert_eq!(
            parse_error_stop_reason(err),
            StopReason::InvalidContract.as_str()
        );
    }

    #[tokio::test]
    async fn manage_context_apply_rejects_state_hash_mismatch() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("u1"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload"),
                    &user_message("u2"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let chunk_id = retrieve
            .json
            .pointer("/chunk_manifest/0/chunk_id")
            .and_then(Value::as_str)
            .expect("first chunk id")
            .to_string();

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": "mismatch",
            "chunk_summaries": [{
                "chunk_id": chunk_id,
                "tool_context": "tool summary",
                "reasoning_context": "reasoning summary"
            }]
        }))
        .expect("parse apply args");

        let err = handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect_err("must fail on stale hash");

        assert_eq!(
            parse_error_stop_reason(err),
            StopReason::StateHashMismatch.as_str()
        );
    }

    #[tokio::test]
    async fn manage_context_apply_rejects_plan_id_mismatch() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();
        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("u1"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload"),
                    &user_message("u2"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();
        let chunk_id = retrieve
            .json
            .pointer("/chunk_manifest/0/chunk_id")
            .and_then(Value::as_str)
            .expect("first chunk id")
            .to_string();

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": "bad-plan-id",
            "state_hash": state_hash,
            "chunk_summaries": [{
                "chunk_id": chunk_id,
                "tool_context": "tool summary",
                "reasoning_context": "reasoning summary"
            }]
        }))
        .expect("parse apply args");

        let err = handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect_err("must fail on invalid plan_id");

        assert_eq!(
            parse_error_stop_reason(err),
            StopReason::PlanIdInvalid.as_str()
        );
    }

    #[tokio::test]
    async fn manage_context_apply_generates_one_context_pair_per_chunk() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("u1"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload 1"),
                    &tool_call("call_tool_2"),
                    &tool_output("call_tool_2", "tool output payload 2"),
                    &user_message("u2"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();

        let manifest = retrieve
            .json
            .get("chunk_manifest")
            .and_then(Value::as_array)
            .expect("chunk_manifest array");
        let selected_chunk_ids = manifest
            .iter()
            .take(2)
            .filter_map(|entry| entry.get("chunk_id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            !selected_chunk_ids.is_empty(),
            "retrieve should return at least one chunk"
        );
        let selected_source_ids = manifest
            .iter()
            .take(selected_chunk_ids.len())
            .filter_map(|entry| entry.get("source_id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        let chunk_summaries = selected_chunk_ids
            .iter()
            .enumerate()
            .map(|(idx, chunk_id)| {
                json!({
                    "chunk_id": chunk_id,
                    "tool_context": format!("tool summary {idx}"),
                    "reasoning_context": format!("reasoning summary {idx}"),
                })
            })
            .collect::<Vec<_>>();

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": state_hash,
            "chunk_summaries": chunk_summaries,
        }))
        .expect("parse apply args");

        let result = handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect("apply");

        assert_eq!(
            result.json.get("mode").and_then(Value::as_str),
            Some("apply")
        );
        assert!(
            result
                .json
                .get("stop_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| {
                    reason == StopReason::TargetReached.as_str()
                        || reason == StopReason::FixedPointReached.as_str()
                }),
            "expected apply stop_reason in {{target_reached, fixed_point_reached}}"
        );
        assert!(
            result
                .json
                .get("new_state_hash")
                .is_some_and(Value::is_string)
        );
        let excluded_chunks = result
            .json
            .pointer("/progress_report/excluded_chunks")
            .and_then(Value::as_u64)
            .expect("excluded_chunks");
        let replaced_chunks = result
            .json
            .pointer("/progress_report/replaced_chunks")
            .and_then(Value::as_u64)
            .expect("replaced_chunks");
        assert!(
            excluded_chunks + replaced_chunks >= selected_chunk_ids.len() as u64,
            "each applied chunk must be excluded or replaced"
        );

        let applied_events = result
            .json
            .get("applied_events")
            .and_then(Value::as_array)
            .expect("applied_events array");
        assert_eq!(applied_events.len(), selected_chunk_ids.len());

        for (idx, event) in applied_events.iter().enumerate() {
            let chunk_id = &selected_chunk_ids[idx];
            assert_eq!(
                event.get("chunk_id").and_then(Value::as_str),
                Some(chunk_id.as_str())
            );

            let tool_context = event
                .get("tool_context")
                .and_then(Value::as_str)
                .expect("tool_context");
            assert!(tool_context.starts_with(TOOL_CONTEXT_OPEN_TAG));
            assert!(tool_context.contains(&format!("chunk_id={chunk_id}")));

            let reasoning_context = event
                .get("reasoning_context")
                .and_then(Value::as_str)
                .expect("reasoning_context");
            assert!(reasoning_context.starts_with(REASONING_CONTEXT_OPEN_TAG));
            assert!(reasoning_context.contains(&format!("chunk_id={chunk_id}")));

            let excluded = event
                .get("excluded")
                .and_then(Value::as_bool)
                .expect("excluded");
            let replacement_applied = event
                .get("replacement_applied")
                .and_then(Value::as_bool)
                .expect("replacement_applied");
            assert!(
                excluded ^ replacement_applied,
                "each applied chunk must be excluded or replaced exactly once"
            );
        }

        let state = session.state.lock().await;
        let history = state.history_snapshot_lenient();

        let mut tool_context_count = 0usize;
        let mut reasoning_context_count = 0usize;

        for item in history {
            let ResponseItem::Message { role, content, .. } = item else {
                continue;
            };
            if role != "user" {
                continue;
            }
            let Some(text) = first_text(&content) else {
                continue;
            };

            for chunk_id in &selected_chunk_ids {
                if text.starts_with(TOOL_CONTEXT_OPEN_TAG)
                    && text.contains(&format!("chunk_id={chunk_id}"))
                {
                    tool_context_count += 1;
                }
                if text.starts_with(REASONING_CONTEXT_OPEN_TAG)
                    && text.contains(&format!("chunk_id={chunk_id}"))
                {
                    reasoning_context_count += 1;
                }
            }
        }

        assert_eq!(tool_context_count, selected_chunk_ids.len());
        assert_eq!(reasoning_context_count, selected_chunk_ids.len());
        drop(state);

        let retrieve_after_apply: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
        }))
        .expect("parse retrieve after apply");
        let after = handle_manage_context(&session, &turn, &retrieve_after_apply)
            .await
            .expect("retrieve after apply");
        let after_manifest = after
            .json
            .get("chunk_manifest")
            .and_then(Value::as_array)
            .expect("after chunk_manifest array");
        let remaining_source_ids: HashSet<String> = after_manifest
            .iter()
            .filter_map(|entry| entry.get("source_id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect();
        for applied_source_id in selected_source_ids {
            assert!(
                !remaining_source_ids.contains(&applied_source_id),
                "applied source_id should no longer be in chunk_manifest: {applied_source_id}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manage_context_apply_follow_up_request_keeps_context_notes_without_completed_manage_context_chatter()
     {
        let server = MockServer::start().await;
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let response_body = sse(&[
            response_created("resp-1"),
            assistant_message_event("msg-1", "done"),
            response_completed("resp-1"),
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(CaptureResponsesRequestResponder {
                calls: AtomicUsize::new(0),
                requests: Arc::clone(&captured_requests),
                response_body,
            })
            .expect(1)
            .mount(&server)
            .await;

        let (mut session, turn, rx) = crate::codex::make_session_and_context_with_rx().await;
        let mut provider = crate::built_in_model_providers()["openai"].clone();
        provider.base_url = Some(format!("{}/v1", server.uri()));

        let session_mut = Arc::get_mut(&mut session).expect("session arc should be unique");
        let auth_manager = Arc::clone(&session_mut.services.auth_manager);
        session_mut.services.model_client = ModelClient::new(
            Some(auth_manager),
            session_mut.conversation_id,
            provider,
            turn.session_source.clone(),
            turn.config.model_verbosity,
            crate::ws_version_from_features(turn.config.as_ref()),
            turn.config
                .features
                .enabled(Feature::EnableRequestCompression),
            turn.config.features.enabled(Feature::RuntimeMetrics),
            None,
        );

        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();
        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("u1"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload"),
                    &user_message("u2"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");
        let retrieve = handle_manage_context(session.as_ref(), turn.as_ref(), &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();
        let chunk_id = retrieve
            .json
            .pointer("/chunk_manifest/0/chunk_id")
            .and_then(Value::as_str)
            .expect("first chunk id")
            .to_string();

        let apply_call_id = "apply-call";
        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &manage_context_call(apply_call_id),
                    &manage_context_output(
                        apply_call_id,
                        &json!({
                            "mode": "apply",
                            "stop_reason": "target_reached"
                        })
                        .to_string(),
                    ),
                ],
                turn.truncation_policy,
            );
        }

        let tool_summary = "tool summary after apply";
        let reasoning_summary = "reasoning summary after apply";
        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": state_hash,
            "chunk_summaries": [{
                "chunk_id": chunk_id,
                "tool_context": tool_summary,
                "reasoning_context": reasoning_summary
            }]
        }))
        .expect("parse apply args");
        handle_manage_context(session.as_ref(), turn.as_ref(), &apply_args)
            .await
            .expect("apply");

        session
            .spawn_task(
                Arc::clone(&turn),
                vec![UserInput::Text {
                    text: "follow up after apply".to_string(),
                    text_elements: Vec::new(),
                }],
                RegularTask::default(),
            )
            .await;
        wait_for_turn_complete(&rx).await;

        let requests = captured_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(requests.len(), 1);

        let body = &requests[0];
        let body_text = body.to_string();
        assert!(
            body_text.contains(tool_summary),
            "follow-up request should include applied tool summary: {body:#?}"
        );
        assert!(
            body_text.contains(reasoning_summary),
            "follow-up request should include applied reasoning summary: {body:#?}"
        );
        assert!(
            body_text.contains("follow up after apply"),
            "follow-up request should include the new user input: {body:#?}"
        );
        assert!(
            !request_contains_manage_context_pair(body, apply_call_id),
            "follow-up request should not contain completed manage_context chatter: {body:#?}"
        );
    }

    #[tokio::test]
    async fn manage_context_apply_persists_compacted_replacement_history_for_resume() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        let recorder = crate::rollout::RolloutRecorder::new(
            turn.config.as_ref(),
            crate::rollout::RolloutRecorderParams::new(
                session.conversation_id,
                None,
                turn.session_source.clone(),
                codex_protocol::models::BaseInstructions::default(),
                Vec::new(),
                crate::rollout::policy::EventPersistenceMode::Limited,
            ),
            None,
            None,
        )
        .await
        .expect("create rollout recorder");
        let rollout_path = recorder.rollout_path().to_path_buf();
        {
            let mut guard = session.services.rollout.lock().await;
            *guard = Some(recorder);
        }

        session
            .persist_rollout_items(&[RolloutItem::EventMsg(
                codex_protocol::protocol::EventMsg::UserMessage(
                    codex_protocol::protocol::UserMessageEvent {
                        message: "seed-user-message".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    },
                ),
            )])
            .await;
        session.ensure_rollout_materialized().await;

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("u1"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload"),
                    &user_message("u2"),
                ],
                turn.truncation_policy,
            );
        }

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");
        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();
        let chunk_id = retrieve
            .json
            .pointer("/chunk_manifest/0/chunk_id")
            .and_then(Value::as_str)
            .expect("first chunk id")
            .to_string();

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": state_hash,
            "chunk_summaries": [{
                "chunk_id": chunk_id,
                "tool_context": "tool summary",
                "reasoning_context": "reasoning summary"
            }]
        }))
        .expect("parse apply args");
        handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect("apply");

        session.flush_rollout().await;
        let (rollout_items, _thread_id, parse_errors) =
            crate::rollout::RolloutRecorder::load_rollout_items(rollout_path.as_path())
                .await
                .expect("load rollout items");
        assert_eq!(parse_errors, 0);
        let replacement_history = rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        });
        assert!(
            replacement_history.is_some_and(|history| !history.is_empty()),
            "manage_context.apply must persist compacted replacement_history for resume replay"
        );
    }

    #[tokio::test]
    async fn manage_context_apply_rollout_persist_failure_rolls_back_state() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        let recorder = crate::rollout::RolloutRecorder::new(
            turn.config.as_ref(),
            crate::rollout::RolloutRecorderParams::new(
                session.conversation_id,
                None,
                turn.session_source.clone(),
                codex_protocol::models::BaseInstructions::default(),
                Vec::new(),
                crate::rollout::policy::EventPersistenceMode::Limited,
            ),
            None,
            None,
        )
        .await
        .expect("create rollout recorder");
        recorder
            .shutdown()
            .await
            .expect("shutdown rollout recorder");
        {
            let mut guard = session.services.rollout.lock().await;
            *guard = Some(recorder);
        }

        {
            let mut state = session.state.lock().await;
            state.record_items(
                [
                    &user_message("u1"),
                    &tool_call("call_tool_1"),
                    &tool_output("call_tool_1", "tool output payload"),
                    &user_message("u2"),
                ],
                turn.truncation_policy,
            );
        }

        let (baseline_history, baseline_overlay, baseline_context_items, baseline_rids) = {
            let state = session.state.lock().await;
            (
                state.history_snapshot_lenient(),
                state.context_overlay_snapshot(),
                state.build_context_items_event(),
                state.history_rids_snapshot_lenient(),
            )
        };

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");
        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();
        let chunk_id = retrieve
            .json
            .pointer("/chunk_manifest/0/chunk_id")
            .and_then(Value::as_str)
            .expect("first chunk id")
            .to_string();

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": state_hash,
            "chunk_summaries": [{
                "chunk_id": chunk_id,
                "tool_context": "tool summary",
                "reasoning_context": "reasoning summary"
            }]
        }))
        .expect("parse apply args");

        let err = handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect_err("apply should fail when rollout persistence cannot enqueue");
        assert_eq!(
            parse_error_stop_reason(err),
            StopReason::RolloutPersistError.as_str()
        );

        let (after_history, after_overlay, after_context_items, after_rids) = {
            let state = session.state.lock().await;
            (
                state.history_snapshot_lenient(),
                state.context_overlay_snapshot(),
                state.build_context_items_event(),
                state.history_rids_snapshot_lenient(),
            )
        };
        assert_eq!(after_history, baseline_history);
        assert_eq!(after_overlay, baseline_overlay);
        assert_eq!(after_context_items, baseline_context_items);
        assert_eq!(after_rids, baseline_rids);
    }

    #[tokio::test]
    async fn manage_context_apply_rejects_invalid_chunk_id() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let policy_id = turn.config.manage_context_policy.quality_rubric_id.clone();

        let retrieve_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "retrieve",
            "policy_id": policy_id,
        }))
        .expect("parse retrieve args");

        let retrieve = handle_manage_context(&session, &turn, &retrieve_args)
            .await
            .expect("retrieve");

        let plan_id = retrieve
            .json
            .get("plan_id")
            .and_then(Value::as_str)
            .expect("plan_id")
            .to_string();
        let state_hash = retrieve
            .json
            .get("state_hash")
            .and_then(Value::as_str)
            .expect("state_hash")
            .to_string();

        let apply_args: ManageContextToolArgs = serde_json::from_value(json!({
            "mode": "apply",
            "policy_id": turn.config.manage_context_policy.quality_rubric_id,
            "plan_id": plan_id,
            "state_hash": state_hash,
            "chunk_summaries": [{
                "chunk_id": "chunk_999",
                "tool_context": "tool summary",
                "reasoning_context": "reasoning summary"
            }]
        }))
        .expect("parse apply args");

        let err = handle_manage_context(&session, &turn, &apply_args)
            .await
            .expect_err("must fail with unknown chunk id");

        let stop_reason = parse_error_stop_reason(err);
        assert_eq!(stop_reason, StopReason::InvalidSummarySchema.as_str());
    }

    #[test]
    fn manage_context_state_hash_ignores_manage_context_call_and_output() {
        let overlay = ContextOverlay::default();

        let items = vec![user_message("u1"), user_message("u2")];
        let ev = ContextItemsEvent {
            items: vec![
                crate::state::ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
            ],
        };
        let base = state_hash_for_context(&ev, &overlay, &items);

        let call_id = "call_manage_context";
        let items_with_manage_context = vec![
            user_message("u1"),
            user_message("u2"),
            ResponseItem::FunctionCall {
                id: None,
                name: "manage_context".to_string(),
                arguments: "{}".to_string(),
                call_id: call_id.to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: call_id.to_string(),
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("{\"ok\":true}".to_string()),
                    success: Some(true),
                },
            },
        ];
        let ev_with_manage_context = ContextItemsEvent {
            items: vec![
                crate::state::ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 2,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 3,
                    category: PruneCategory::ToolOutput,
                    preview: String::new(),
                    included: true,
                    id: Some("r3".to_string()),
                },
            ],
        };
        let extended = state_hash_for_context(
            &ev_with_manage_context,
            &overlay,
            &items_with_manage_context,
        );

        assert_eq!(base, extended);
    }

    #[test]
    fn manage_context_state_hash_keeps_output_on_function_call_id_collision() {
        let overlay = ContextOverlay::default();

        let items_with_output = vec![
            user_message("u1"),
            manage_context_call("shared"),
            tool_call("shared"),
            tool_output("shared", "ok"),
        ];
        let ev_with_output = ContextItemsEvent {
            items: vec![
                crate::state::ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 1,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 2,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 3,
                    category: PruneCategory::ToolOutput,
                    preview: String::new(),
                    included: true,
                    id: Some("r3".to_string()),
                },
            ],
        };
        let with_output_hash =
            state_hash_for_context(&ev_with_output, &overlay, &items_with_output);

        let items_without_output = vec![
            user_message("u1"),
            manage_context_call("shared"),
            tool_call("shared"),
        ];
        let ev_without_output = ContextItemsEvent {
            items: vec![
                crate::state::ContextItemSummary {
                    index: 0,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 1,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 2,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
            ],
        };
        let without_output_hash =
            state_hash_for_context(&ev_without_output, &overlay, &items_without_output);

        assert_ne!(with_output_hash, without_output_hash);
    }

    #[test]
    fn manage_context_state_hash_ignores_items_appended_after_latest_user_message() {
        let overlay = ContextOverlay::default();

        let items = vec![
            assistant_message("older assistant"),
            user_message("latest user"),
        ];
        let ev = ContextItemsEvent {
            items: vec![
                crate::state::ContextItemSummary {
                    index: 0,
                    category: PruneCategory::AssistantMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
            ],
        };
        let base = state_hash_for_context(&ev, &overlay, &items);

        let appended_items = vec![
            assistant_message("older assistant"),
            user_message("latest user"),
            assistant_message("appended assistant"),
            tool_call("call_tool_1"),
            tool_output("call_tool_1", "appended tool output"),
        ];
        let ev_with_append = ContextItemsEvent {
            items: vec![
                crate::state::ContextItemSummary {
                    index: 0,
                    category: PruneCategory::AssistantMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r0".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 1,
                    category: PruneCategory::UserMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r1".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 2,
                    category: PruneCategory::AssistantMessage,
                    preview: String::new(),
                    included: true,
                    id: Some("r2".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 3,
                    category: PruneCategory::ToolCall,
                    preview: String::new(),
                    included: true,
                    id: Some("r3".to_string()),
                },
                crate::state::ContextItemSummary {
                    index: 4,
                    category: PruneCategory::ToolOutput,
                    preview: String::new(),
                    included: true,
                    id: Some("r4".to_string()),
                },
            ],
        };
        let extended = state_hash_for_context(&ev_with_append, &overlay, &appended_items);

        assert_eq!(base, extended);
    }

    fn first_text(content: &[codex_protocol::models::ContentItem]) -> Option<&str> {
        for item in content {
            match item {
                codex_protocol::models::ContentItem::InputText { text }
                | codex_protocol::models::ContentItem::OutputText { text } => return Some(text),
                codex_protocol::models::ContentItem::InputImage { .. } => {}
            }
        }
        None
    }
}
