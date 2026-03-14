use crate::codex::Session;
use crate::codex::TurnContext;
use crate::prompt_gc_sidecar::MAX_RAW_BYTES_PER_RETRIEVE;
use crate::prompt_gc_sidecar::MAX_UNITS_PER_RETRIEVE;
use crate::prompt_gc_sidecar::PromptGcCapturedUnit;
use crate::prompt_gc_sidecar::PromptGcCheckpoint;
use crate::prompt_gc_sidecar::PromptGcUnitKind;
use crate::prompt_gc_sidecar::PromptGcUnitResolver;
use crate::protocol::REASONING_CONTEXT_CLOSE_TAG;
use crate::protocol::REASONING_CONTEXT_OPEN_TAG;
use crate::protocol::TOOL_CONTEXT_CLOSE_TAG;
use crate::protocol::TOOL_CONTEXT_OPEN_TAG;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha1::Digest;
use std::collections::HashMap;
use std::collections::HashSet;

pub(crate) struct PromptGcHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptGcToolArgs {
    mode: String,
    #[serde(default)]
    policy_id: Option<String>,
    #[serde(default)]
    checkpoint_id: Option<String>,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    state_hash: Option<String>,
    #[serde(default)]
    chunk_summaries: Option<Vec<PromptGcChunkSummaryInput>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptGcChunkSummaryInput {
    chunk_id: String,
    tool_context: String,
    reasoning_context: String,
}

#[derive(Debug, Clone, Serialize)]
struct PromptGcChunkManifestEntry {
    chunk_id: String,
    unit_key: u64,
    kind: String,
    approx_bytes: usize,
    payload_text: String,
    call_name: Option<String>,
}

#[derive(Debug, Clone)]
struct PromptGcResolvedChunk {
    manifest: PromptGcChunkManifestEntry,
    exclusion_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
struct PromptGcRetrievePlan {
    plan_id: String,
    state_hash: String,
    chunk_manifest: Vec<PromptGcResolvedChunk>,
}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    TargetReached,
    InvalidContract,
    InvalidSummarySchema,
    StateHashMismatch,
    PlanIdInvalid,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TargetReached => "target_reached",
            Self::InvalidContract => "invalid_contract",
            Self::InvalidSummarySchema => "invalid_summary_schema",
            Self::StateHashMismatch => "state_hash_mismatch",
            Self::PlanIdInvalid => "plan_id_invalid",
        }
    }
}

#[async_trait]
impl ToolHandler for PromptGcHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, crate::function_tool::FunctionCallError> {
        let ToolInvocation {
            session,
            payload,
            turn,
            ..
        } = invocation;

        let ToolPayload::Function { arguments } = payload else {
            return Err(contract_error(
                StopReason::InvalidContract,
                "prompt_gc handler received unsupported payload",
            ));
        };

        let args: PromptGcToolArgs = serde_json::from_str(&arguments).map_err(|error| {
            contract_error(
                StopReason::InvalidContract,
                format!("failed to parse function arguments: {error}"),
            )
        })?;

        let result = handle_prompt_gc(session.as_ref(), turn.as_ref(), &args).await?;
        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(
                serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
            ),
            success: Some(true),
        })
    }
}

async fn handle_prompt_gc(
    session: &Session,
    turn: &TurnContext,
    args: &PromptGcToolArgs,
) -> Result<serde_json::Value, crate::function_tool::FunctionCallError> {
    match args.mode.as_str() {
        "retrieve" => handle_retrieve(session, turn, args).await,
        "apply" => handle_apply(session, turn, args).await,
        other => Err(contract_error(
            StopReason::InvalidContract,
            format!("unsupported prompt_gc mode: {other}"),
        )),
    }
}

async fn handle_retrieve(
    session: &Session,
    turn: &TurnContext,
    args: &PromptGcToolArgs,
) -> Result<serde_json::Value, crate::function_tool::FunctionCallError> {
    if args.plan_id.is_some() || args.state_hash.is_some() || args.chunk_summaries.is_some() {
        return Err(contract_error(
            StopReason::InvalidContract,
            "prompt_gc.retrieve accepts only mode, policy_id, and checkpoint_id",
        ));
    }

    validate_policy_id(
        required_non_empty_str("policy_id", args.policy_id.as_ref())?,
        turn,
    )?;
    let checkpoint_id = required_non_empty_str("checkpoint_id", args.checkpoint_id.as_ref())?;
    let plan = build_retrieve_plan(session, turn, checkpoint_id).await?;
    Ok(json!({
        "mode": "retrieve",
        "policy_id": turn.config.manage_context_policy.quality_rubric_id,
        "checkpoint_id": checkpoint_id,
        "plan_id": plan.plan_id,
        "state_hash": plan.state_hash,
        "chunk_manifest": plan.chunk_manifest.iter().map(|chunk| &chunk.manifest).collect::<Vec<_>>(),
        "progress_report": {
            "selected_chunks": plan.chunk_manifest.len(),
            "max_units_per_apply": MAX_UNITS_PER_RETRIEVE,
            "max_raw_bytes_per_retrieve": MAX_RAW_BYTES_PER_RETRIEVE,
        }
    }))
}

async fn handle_apply(
    session: &Session,
    turn: &TurnContext,
    args: &PromptGcToolArgs,
) -> Result<serde_json::Value, crate::function_tool::FunctionCallError> {
    validate_policy_id(
        required_non_empty_str("policy_id", args.policy_id.as_ref())?,
        turn,
    )?;
    let checkpoint_id = required_non_empty_str("checkpoint_id", args.checkpoint_id.as_ref())?;
    let plan_id = required_non_empty_str("plan_id", args.plan_id.as_ref())?;
    let state_hash = required_non_empty_str("state_hash", args.state_hash.as_ref())?;
    let chunk_summaries = args.chunk_summaries.as_ref().ok_or_else(|| {
        contract_error(
            StopReason::InvalidContract,
            "prompt_gc.apply requires chunk_summaries",
        )
    })?;
    if chunk_summaries.is_empty() {
        return Err(contract_error(
            StopReason::InvalidContract,
            "prompt_gc.apply requires a non-empty chunk_summaries list",
        ));
    }

    let plan = build_retrieve_plan(session, turn, checkpoint_id).await?;
    validate_chunk_summaries(chunk_summaries, &plan.chunk_manifest)?;
    if plan.plan_id != plan_id {
        return Err(contract_error(
            StopReason::PlanIdInvalid,
            format!(
                "plan_id mismatch (expected '{}', got '{plan_id}')",
                plan.plan_id
            ),
        ));
    }
    if plan.state_hash != state_hash {
        return Err(contract_error(
            StopReason::StateHashMismatch,
            format!(
                "state_hash mismatch (expected '{}', got '{state_hash}')",
                plan.state_hash
            ),
        ));
    }

    let selected_chunks = select_chunks(&plan, chunk_summaries)?;
    let notes = build_notes(chunk_summaries)?;
    let mut exclusion_indices = selected_chunks
        .iter()
        .flat_map(|chunk| chunk.exclusion_indices.iter().copied())
        .collect::<Vec<_>>();
    exclusion_indices.sort_unstable();
    exclusion_indices.dedup();
    let applied_unit_keys = selected_chunks
        .iter()
        .map(|chunk| chunk.manifest.unit_key)
        .collect::<Vec<_>>();

    let replacement_history = {
        let mut state = session.state.lock().await;
        let checkpoint = state.manage_context_checkpoint();
        state.set_context_inclusion(&exclusion_indices, false);
        state.add_context_notes(notes);
        let replacement_history = state.prompt_snapshot_lenient();
        state.restore_manage_context_checkpoint(checkpoint.clone());
        replacement_history
    };

    session
        .persist_prompt_gc_replacement_history(turn, replacement_history)
        .await
        .map_err(|error| {
            contract_error(
                StopReason::InvalidContract,
                format!("prompt_gc apply failed to persist replacement history: {error}"),
            )
        })?;

    if let Some(sidecar) = session.prompt_gc_sidecar_for_sub_id(&turn.sub_id).await {
        // Merge-safety anchor: hidden prompt_gc apply can succeed even if the model stream ends
        // before response.completed. Cache the committed apply outcome in the sidecar so the
        // runner can recover the cycle without relying on a streamed terminal tool output.
        sidecar
            .lock()
            .await
            .note_apply_outcome(checkpoint_id, applied_unit_keys.clone());
    }

    Ok(json!({
        "mode": "apply",
        "checkpoint_id": checkpoint_id,
        "applied_unit_keys": applied_unit_keys,
        "stop_reason": StopReason::TargetReached.as_str(),
        "progress_report": {
            "applied_chunks": selected_chunks.len(),
            "applied_notes": chunk_summaries.len(),
        }
    }))
}

fn select_chunks(
    plan: &PromptGcRetrievePlan,
    chunk_summaries: &[PromptGcChunkSummaryInput],
) -> Result<Vec<PromptGcResolvedChunk>, crate::function_tool::FunctionCallError> {
    let by_chunk_id = plan
        .chunk_manifest
        .iter()
        .cloned()
        .map(|chunk| (chunk.manifest.chunk_id.clone(), chunk))
        .collect::<HashMap<_, _>>();
    let mut selected = Vec::with_capacity(chunk_summaries.len());
    for summary in chunk_summaries {
        let Some(chunk) = by_chunk_id.get(&summary.chunk_id) else {
            return Err(contract_error(
                StopReason::InvalidContract,
                format!("unknown chunk_id '{}'", summary.chunk_id),
            ));
        };
        selected.push(chunk.clone());
    }
    Ok(selected)
}

fn validate_chunk_summaries(
    chunk_summaries: &[PromptGcChunkSummaryInput],
    chunk_manifest: &[PromptGcResolvedChunk],
) -> Result<(), crate::function_tool::FunctionCallError> {
    // Merge-safety anchor: prompt_gc.apply must stay fail-loud on duplicate
    // chunk_id payloads just like manage_context.apply. Silent duplicates can
    // inject conflicting contextual notes for the same hidden chunk.
    let manifest_ids: HashSet<&str> = chunk_manifest
        .iter()
        .map(|entry| entry.manifest.chunk_id.as_str())
        .collect();
    let mut seen_chunk_ids: HashSet<&str> = HashSet::new();

    for chunk in chunk_summaries {
        let chunk_id = chunk.chunk_id.trim();
        if chunk_id.is_empty() {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                "prompt_gc.apply chunk_summaries[].chunk_id must be non-empty",
            ));
        }
        if chunk.tool_context.trim().is_empty() && chunk.reasoning_context.trim().is_empty() {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                format!("chunk '{chunk_id}' summary is empty"),
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

fn build_notes(
    chunk_summaries: &[PromptGcChunkSummaryInput],
) -> Result<Vec<String>, crate::function_tool::FunctionCallError> {
    let mut notes = Vec::new();
    for summary in chunk_summaries {
        let chunk_id = summary.chunk_id.trim();
        let tool_context = summary.tool_context.trim();
        let reasoning_context = summary.reasoning_context.trim();
        if tool_context.is_empty() && reasoning_context.is_empty() {
            return Err(contract_error(
                StopReason::InvalidSummarySchema,
                format!("chunk '{chunk_id}' summary is empty"),
            ));
        }
        if !tool_context.is_empty() {
            notes.push(format!(
                "{TOOL_CONTEXT_OPEN_TAG}\nchunk_id={chunk_id}\n{tool_context}\n{TOOL_CONTEXT_CLOSE_TAG}"
            ));
        }
        if !reasoning_context.is_empty() {
            notes.push(format!(
                "{REASONING_CONTEXT_OPEN_TAG}\nchunk_id={chunk_id}\n{reasoning_context}\n{REASONING_CONTEXT_CLOSE_TAG}"
            ));
        }
    }
    Ok(notes)
}

async fn build_retrieve_plan(
    session: &Session,
    turn: &TurnContext,
    checkpoint_id: &str,
) -> Result<PromptGcRetrievePlan, crate::function_tool::FunctionCallError> {
    let sidecar = session
        .prompt_gc_sidecar_for_sub_id(&turn.sub_id)
        .await
        .ok_or_else(|| {
            contract_error(
                StopReason::InvalidContract,
                "prompt_gc sidecar is not active for this turn",
            )
        })?;
    let sidecar = sidecar.lock().await;
    let checkpoint = sidecar.checkpoint(checkpoint_id).ok_or_else(|| {
        contract_error(
            StopReason::InvalidContract,
            format!("unknown checkpoint_id '{checkpoint_id}'"),
        )
    })?;
    let units = sidecar
        .selectable_units(
            checkpoint_id,
            MAX_UNITS_PER_RETRIEVE,
            MAX_RAW_BYTES_PER_RETRIEVE,
        )
        .unwrap_or_default();
    drop(sidecar);

    let current_history = {
        let state = session.state.lock().await;
        state.history_snapshot_lenient()
    };

    let mut chunk_manifest = Vec::with_capacity(units.len());
    let mut search_cursor = 0usize;
    for unit in units {
        let exclusion_indices = resolve_unit_indices(&current_history, &unit, &mut search_cursor)?;
        chunk_manifest.push(PromptGcResolvedChunk {
            manifest: PromptGcChunkManifestEntry {
                chunk_id: unit.chunk_id.clone(),
                unit_key: unit.unit_key,
                kind: match unit.kind {
                    PromptGcUnitKind::Reasoning => "reasoning".to_string(),
                    PromptGcUnitKind::ToolPair => "tool_pair".to_string(),
                    PromptGcUnitKind::ToolResult => "tool_result".to_string(),
                },
                approx_bytes: unit.approx_bytes,
                payload_text: unit.payload_text.clone(),
                call_name: match &unit.resolver {
                    PromptGcUnitResolver::Reasoning { .. } => None,
                    PromptGcUnitResolver::ToolPair { call_name, .. } => Some(call_name.clone()),
                    PromptGcUnitResolver::ToolResult { call_name, .. } => Some(call_name.clone()),
                },
            },
            exclusion_indices,
        });
    }
    let state_hash = state_hash_for(&checkpoint, &chunk_manifest);
    let plan_id = plan_id_for(
        turn.config.manage_context_policy.quality_rubric_id.as_str(),
        &state_hash,
        &chunk_manifest,
    );
    Ok(PromptGcRetrievePlan {
        plan_id,
        state_hash,
        chunk_manifest,
    })
}

fn resolve_unit_indices(
    current_history: &[ResponseItem],
    unit: &PromptGcCapturedUnit,
    search_cursor: &mut usize,
) -> Result<Vec<usize>, crate::function_tool::FunctionCallError> {
    // Merge-safety anchor: prompt-gc resolvers must match against the current
    // rewritten history in observation order. Storing absolute indices across
    // apply cycles breaks partial compaction after history replacement.
    match &unit.resolver {
        PromptGcUnitResolver::Reasoning { fingerprint } => {
            let history_index = resolve_item_index(current_history, *search_cursor, fingerprint)
                .ok_or_else(|| {
                    contract_error(
                        StopReason::StateHashMismatch,
                        format!("reasoning unit {} drifted before apply", unit.unit_key),
                    )
                })?;
            *search_cursor = history_index.saturating_add(1);
            Ok(vec![history_index])
        }
        PromptGcUnitResolver::ToolPair {
            call_id,
            call_fingerprint,
            output_fingerprint,
            ..
        } => {
            let call_index = resolve_item_index(current_history, *search_cursor, call_fingerprint)
                .ok_or_else(|| {
                    contract_error(
                        StopReason::StateHashMismatch,
                        format!("tool call for unit {} drifted before apply", unit.unit_key),
                    )
                })?;
            let output_index = resolve_function_output_index(
                current_history,
                call_index.saturating_add(1),
                call_id,
                output_fingerprint,
            )
            .ok_or_else(|| {
                contract_error(
                    StopReason::StateHashMismatch,
                    format!(
                        "tool output for unit {} drifted before apply",
                        unit.unit_key
                    ),
                )
            })?;
            *search_cursor = output_index.saturating_add(1);
            Ok(vec![call_index, output_index])
        }
        PromptGcUnitResolver::ToolResult { fingerprint, .. } => {
            let history_index = resolve_item_index(current_history, *search_cursor, fingerprint)
                .ok_or_else(|| {
                    contract_error(
                        StopReason::StateHashMismatch,
                        format!("tool result unit {} drifted before apply", unit.unit_key),
                    )
                })?;
            *search_cursor = history_index.saturating_add(1);
            Ok(vec![history_index])
        }
    }
}

fn resolve_item_index(
    current_history: &[ResponseItem],
    start_index: usize,
    fingerprint: &str,
) -> Option<usize> {
    current_history
        .iter()
        .enumerate()
        .skip(start_index)
        .find_map(|(index, item)| {
            let current = serde_json::to_string(item).unwrap_or_default();
            (current == fingerprint).then_some(index)
        })
}

fn resolve_function_output_index(
    current_history: &[ResponseItem],
    start_index: usize,
    call_id: &str,
    fingerprint: &str,
) -> Option<usize> {
    current_history
        .iter()
        .enumerate()
        .skip(start_index)
        .find_map(|(index, item)| match item {
            ResponseItem::FunctionCallOutput {
                call_id: current_call_id,
                ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id: current_call_id,
                ..
            } if current_call_id == call_id => {
                let current = serde_json::to_string(item).unwrap_or_default();
                (current == fingerprint).then_some(index)
            }
            _ => None,
        })
}

fn state_hash_for(
    checkpoint: &PromptGcCheckpoint,
    chunk_manifest: &[PromptGcResolvedChunk],
) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(checkpoint.checkpoint_id.as_bytes());
    hasher.update(checkpoint.checkpoint_seq.to_string().as_bytes());
    hasher.update(checkpoint.eligible_unit_count.to_string().as_bytes());
    for chunk in chunk_manifest {
        hasher.update(chunk.manifest.chunk_id.as_bytes());
        hasher.update(chunk.manifest.unit_key.to_string().as_bytes());
        hasher.update(chunk.manifest.kind.as_bytes());
        hasher.update(chunk.manifest.payload_text.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn plan_id_for(
    policy_id: &str,
    state_hash: &str,
    chunk_manifest: &[PromptGcResolvedChunk],
) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(policy_id.as_bytes());
    hasher.update(state_hash.as_bytes());
    for chunk in chunk_manifest {
        hasher.update(chunk.manifest.chunk_id.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn required_non_empty_str<'a>(
    field_name: &str,
    value: Option<&'a String>,
) -> Result<&'a str, crate::function_tool::FunctionCallError> {
    let value = value
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            contract_error(
                StopReason::InvalidContract,
                format!("missing or empty '{field_name}'"),
            )
        })?;
    Ok(value)
}

fn validate_policy_id(
    policy_id: &str,
    turn: &TurnContext,
) -> Result<(), crate::function_tool::FunctionCallError> {
    let expected = turn.config.manage_context_policy.quality_rubric_id.trim();
    if policy_id != expected {
        return Err(contract_error(
            StopReason::InvalidContract,
            format!("policy_id mismatch (expected '{expected}', got '{policy_id}')"),
        ));
    }
    Ok(())
}

fn contract_error(
    stop_reason: StopReason,
    message: impl Into<String>,
) -> crate::function_tool::FunctionCallError {
    crate::function_tool::FunctionCallError::RespondToModel(
        json!({
            "mode": "error",
            "stop_reason": stop_reason.as_str(),
            "message": message.into(),
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use crate::protocol::TokenUsage;
    use crate::state::ActiveTurn;
    use crate::state::RunningTask;
    use crate::state::TaskKind;
    use crate::tasks::RegularTask;
    use crate::tasks::SessionTask;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::LocalShellAction;
    use codex_protocol::models::LocalShellExecAction;
    use codex_protocol::models::LocalShellStatus;
    use codex_protocol::models::MessagePhase;
    use codex_protocol::models::WebSearchAction;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::AbortOnDropHandle;

    async fn install_prompt_gc_active_turn(
        session: &Session,
        turn_context: Arc<TurnContext>,
    ) -> Arc<tokio::sync::Mutex<crate::prompt_gc_sidecar::PromptGcSidecar>> {
        let mut active_turn = ActiveTurn::default();
        let sidecar = active_turn.ensure_prompt_gc_sidecar();
        sidecar.lock().await.bind_turn(turn_context.sub_id.clone());
        active_turn.add_task(RunningTask {
            done: Arc::new(Notify::new()),
            kind: TaskKind::Regular,
            task: Arc::new(RegularTask::default()) as Arc<dyn SessionTask>,
            cancellation_token: CancellationToken::new(),
            handle: Arc::new(AbortOnDropHandle::new(tokio::spawn(async {}))),
            turn_context: Arc::clone(&turn_context),
            _timer: None,
        });
        {
            let mut turn_state = active_turn.turn_state.lock().await;
            turn_state.token_usage_at_turn_start = TokenUsage::default();
        }
        *session.active_turn.lock().await = Some(active_turn);
        sidecar
    }

    fn commentary_phase_message(id: &str) -> ResponseItem {
        ResponseItem::Message {
            id: Some(id.to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "checkpoint".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        }
    }

    fn apply_args(
        turn: &TurnContext,
        checkpoint_id: &str,
        plan: &PromptGcRetrievePlan,
        chunk_summaries: Vec<PromptGcChunkSummaryInput>,
    ) -> PromptGcToolArgs {
        PromptGcToolArgs {
            mode: "apply".to_string(),
            policy_id: Some(turn.config.manage_context_policy.quality_rubric_id.clone()),
            checkpoint_id: Some(checkpoint_id.to_string()),
            plan_id: Some(plan.plan_id.clone()),
            state_hash: Some(plan.state_hash.clone()),
            chunk_summaries: Some(chunk_summaries),
        }
    }

    async fn activate_pending_checkpoint(
        sidecar: &Arc<tokio::sync::Mutex<crate::prompt_gc_sidecar::PromptGcSidecar>>,
    ) -> PromptGcCheckpoint {
        sidecar
            .lock()
            .await
            .take_pending_checkpoint()
            .expect("pending checkpoint")
    }

    #[tokio::test]
    async fn prompt_gc_apply_rejects_duplicate_chunk_summaries() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let items = vec![
            ResponseItem::Reasoning {
                id: "reasoning-1".to_string(),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            },
            commentary_phase_message("phase-1"),
        ];
        session
            .record_conversation_items(turn_context.as_ref(), &items)
            .await;

        let checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let plan = build_retrieve_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");
        let chunk_id = plan.chunk_manifest[0].manifest.chunk_id.clone();

        let error = handle_apply(
            &session,
            turn_context.as_ref(),
            &apply_args(
                turn_context.as_ref(),
                &checkpoint_id,
                &plan,
                vec![
                    PromptGcChunkSummaryInput {
                        chunk_id: chunk_id.clone(),
                        tool_context: "tool".to_string(),
                        reasoning_context: "reasoning".to_string(),
                    },
                    PromptGcChunkSummaryInput {
                        chunk_id,
                        tool_context: "tool".to_string(),
                        reasoning_context: "reasoning".to_string(),
                    },
                ],
            ),
        )
        .await
        .expect_err("duplicate chunk ids must fail");

        let crate::function_tool::FunctionCallError::RespondToModel(message) = error else {
            panic!("expected model-visible contract error");
        };
        assert!(message.contains("invalid_summary_schema"));
        assert!(message.contains("appears more than once"));
    }

    #[tokio::test]
    async fn prompt_gc_retrieve_includes_local_shell_and_single_item_tool_results() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let items = vec![
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("shell-1".to_string()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["pwd".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "shell-1".to_string(),
                output: FunctionCallOutputPayload::from_text("/tmp".to_string()),
            },
            ResponseItem::WebSearchCall {
                id: Some("ws-1".to_string()),
                status: Some("completed".to_string()),
                action: Some(WebSearchAction::Search {
                    query: Some("weather".to_string()),
                    queries: None,
                }),
            },
            ResponseItem::ImageGenerationCall {
                id: "ig-1".to_string(),
                status: "completed".to_string(),
                revised_prompt: Some("cat".to_string()),
                result: "image-ref".to_string(),
            },
            commentary_phase_message("phase-1"),
        ];
        session
            .record_conversation_items(turn_context.as_ref(), &items)
            .await;

        let checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let plan = build_retrieve_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");

        assert_eq!(plan.chunk_manifest.len(), 3);
        assert_eq!(
            plan.chunk_manifest
                .iter()
                .map(|chunk| (
                    chunk.manifest.kind.as_str(),
                    chunk.manifest.call_name.as_deref().unwrap_or_default()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("tool_pair", "local_shell"),
                ("tool_result", "web_search"),
                ("tool_result", "image_generation"),
            ]
        );
    }

    #[tokio::test]
    async fn prompt_gc_partial_apply_allows_later_checkpoint_to_compact_leftovers() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let initial_items = vec![
            ResponseItem::Reasoning {
                id: "reasoning-1".to_string(),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            },
            ResponseItem::Reasoning {
                id: "reasoning-2".to_string(),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            },
            commentary_phase_message("phase-1"),
        ];
        session
            .record_conversation_items(turn_context.as_ref(), &initial_items)
            .await;

        let first_checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let first_plan = build_retrieve_plan(&session, turn_context.as_ref(), &first_checkpoint_id)
            .await
            .expect("first retrieve plan");
        assert_eq!(first_plan.chunk_manifest.len(), 2);

        let first_chunk = first_plan.chunk_manifest[0].manifest.chunk_id.clone();
        let apply_value = handle_apply(
            &session,
            turn_context.as_ref(),
            &apply_args(
                turn_context.as_ref(),
                &first_checkpoint_id,
                &first_plan,
                vec![PromptGcChunkSummaryInput {
                    chunk_id: first_chunk,
                    tool_context: "tool".to_string(),
                    reasoning_context: "reasoning".to_string(),
                }],
            ),
        )
        .await
        .expect("apply");
        let applied_unit_keys = apply_value
            .get("applied_unit_keys")
            .and_then(serde_json::Value::as_array)
            .expect("applied unit keys")
            .iter()
            .filter_map(serde_json::Value::as_u64)
            .collect::<Vec<_>>();
        sidecar
            .lock()
            .await
            .complete_cycle(crate::prompt_gc_sidecar::PromptGcApplyOutcome {
                checkpoint_id: first_checkpoint_id.clone(),
                checkpoint_seq: 0,
                applied_unit_keys,
            });

        session
            .record_conversation_items(
                turn_context.as_ref(),
                &[commentary_phase_message("phase-2")],
            )
            .await;
        let second_checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let second_plan =
            build_retrieve_plan(&session, turn_context.as_ref(), &second_checkpoint_id)
                .await
                .expect("second retrieve plan");

        assert_eq!(second_plan.chunk_manifest.len(), 1);
        assert_eq!(second_plan.chunk_manifest[0].manifest.kind, "reasoning");
        assert!(!session.clone_history().await.raw_items().is_empty());
    }

    #[tokio::test]
    async fn prompt_gc_retrieve_resolves_truncated_tool_outputs_from_canonical_history() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let items = vec![
            ResponseItem::FunctionCall {
                id: None,
                call_id: "call-1".to_string(),
                name: "shell".to_string(),
                arguments: "{\"cmd\":\"pwd\"}".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload::from_text("x".repeat(200_000)),
            },
            commentary_phase_message("phase-1"),
        ];
        session
            .record_conversation_items(turn_context.as_ref(), &items)
            .await;

        let checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let plan = build_retrieve_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan should resolve stored truncated output");

        assert_eq!(plan.chunk_manifest.len(), 1);
        assert_eq!(plan.chunk_manifest[0].manifest.kind, "tool_pair");
    }

    #[tokio::test]
    async fn prompt_gc_apply_keeps_identical_summaries_for_distinct_chunks() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let items = vec![
            ResponseItem::Reasoning {
                id: "reasoning-1".to_string(),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            },
            ResponseItem::Reasoning {
                id: "reasoning-2".to_string(),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            },
            commentary_phase_message("phase-1"),
        ];
        session
            .record_conversation_items(turn_context.as_ref(), &items)
            .await;

        let checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let plan = build_retrieve_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");

        handle_apply(
            &session,
            turn_context.as_ref(),
            &apply_args(
                turn_context.as_ref(),
                &checkpoint_id,
                &plan,
                plan.chunk_manifest
                    .iter()
                    .map(|chunk| PromptGcChunkSummaryInput {
                        chunk_id: chunk.manifest.chunk_id.clone(),
                        tool_context: "same summary".to_string(),
                        reasoning_context: "same summary".to_string(),
                    })
                    .collect(),
            ),
        )
        .await
        .expect("apply");

        let prompt = session.state.lock().await.prompt_snapshot_lenient();
        let tool_notes = prompt
            .iter()
            .filter_map(|item| match item {
                ResponseItem::Message { role, content, .. } if role == "user" => {
                    let ContentItem::InputText { text } = &content[0] else {
                        return None;
                    };
                    text.contains(TOOL_CONTEXT_OPEN_TAG).then_some(text.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(tool_notes.len(), 2);
        assert!(
            tool_notes
                .iter()
                .any(|note| note.contains("chunk_id=prompt_gc_chunk_0"))
        );
        assert!(
            tool_notes
                .iter()
                .any(|note| note.contains("chunk_id=prompt_gc_chunk_1"))
        );
    }
}
