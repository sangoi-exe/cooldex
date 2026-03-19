use crate::codex::Session;
use crate::codex::TurnContext;
use crate::prompt_gc_sidecar::MAX_RAW_BYTES_PER_RETRIEVE;
use crate::prompt_gc_sidecar::MAX_UNITS_PER_RETRIEVE;
use crate::prompt_gc_sidecar::PromptGcApplyOutcome;
use crate::prompt_gc_sidecar::PromptGcCapturedUnit;
use crate::prompt_gc_sidecar::PromptGcCheckpoint;
use crate::prompt_gc_sidecar::PromptGcUnitKind;
use crate::prompt_gc_sidecar::PromptGcUnitResolver;
use crate::protocol::REASONING_CONTEXT_CLOSE_TAG;
use crate::protocol::REASONING_CONTEXT_OPEN_TAG;
use crate::protocol::TOOL_CONTEXT_CLOSE_TAG;
use crate::protocol::TOOL_CONTEXT_OPEN_TAG;
use crate::truncate::TruncationPolicy;
use crate::truncate::truncate_text;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptGcChunkSummary {
    pub(crate) chunk_id: String,
    pub(crate) tool_context: String,
    pub(crate) reasoning_context: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PromptGcChunkManifestEntry {
    pub(crate) chunk_id: String,
    pub(crate) unit_key: u64,
    pub(crate) kind: String,
    pub(crate) approx_bytes: usize,
    pub(crate) payload_text: String,
    pub(crate) call_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PromptGcResolvedChunk {
    pub(crate) manifest: PromptGcChunkManifestEntry,
    pub(crate) exclusion_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct PromptGcRuntimePlan {
    pub(crate) chunk_manifest: Vec<PromptGcResolvedChunk>,
}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    InvalidContract,
    InvalidSummarySchema,
    StateHashMismatch,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidContract => "invalid_contract",
            Self::InvalidSummarySchema => "invalid_summary_schema",
            Self::StateHashMismatch => "state_hash_mismatch",
        }
    }
}

fn select_chunks(
    plan: &PromptGcRuntimePlan,
    chunk_summaries: &[PromptGcChunkSummary],
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

fn manifest_payload_text(unit: &PromptGcCapturedUnit) -> String {
    if unit.approx_bytes <= MAX_RAW_BYTES_PER_RETRIEVE {
        return unit.payload_text.clone();
    }

    // Merge-safety anchor: prompt_gc retrieve must keep oversize first units eligible without
    // feeding the hidden model the full raw payload; preserve canonical approx_bytes for
    // accounting and selection, but bound the manifest preview bytes.
    truncate_text(
        &unit.payload_text,
        TruncationPolicy::Bytes(MAX_RAW_BYTES_PER_RETRIEVE),
    )
}

fn validate_chunk_summaries(
    chunk_summaries: &[PromptGcChunkSummary],
    chunk_manifest: &[PromptGcResolvedChunk],
) -> Result<(), crate::function_tool::FunctionCallError> {
    // Merge-safety anchor: prompt_gc summary validation must stay fail-loud on
    // duplicate chunk_id payloads just like manage_context.apply. Silent
    // duplicates can inject conflicting contextual notes for the same hidden
    // chunk.
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
                "prompt_gc chunk_summaries[].chunk_id must be non-empty",
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
                format!("chunk_id '{chunk_id}' appears more than once in summary payload"),
            ));
        }
    }

    let missing_chunk_ids = chunk_manifest
        .iter()
        .map(|entry| entry.manifest.chunk_id.as_str())
        .filter(|chunk_id| !seen_chunk_ids.contains(chunk_id))
        .collect::<Vec<_>>();
    if !missing_chunk_ids.is_empty() || chunk_summaries.len() != chunk_manifest.len() {
        let missing_chunk_ids = if missing_chunk_ids.is_empty() {
            "<none>".to_string()
        } else {
            missing_chunk_ids.join(", ")
        };
        return Err(contract_error(
            StopReason::InvalidSummarySchema,
            format!(
                "prompt_gc requires summaries for every chunk_manifest entry; expected {}, got {}, missing chunk_id(s): {missing_chunk_ids}",
                chunk_manifest.len(),
                chunk_summaries.len(),
            ),
        ));
    }

    Ok(())
}

fn build_notes(
    chunk_summaries: &[PromptGcChunkSummary],
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

fn order_chunk_summaries(
    chunk_manifest: &[PromptGcResolvedChunk],
    chunk_summaries: &[PromptGcChunkSummary],
) -> Result<Vec<PromptGcChunkSummary>, crate::function_tool::FunctionCallError> {
    let by_chunk_id = chunk_summaries
        .iter()
        .cloned()
        .map(|summary| (summary.chunk_id.clone(), summary))
        .collect::<HashMap<_, _>>();
    chunk_manifest
        .iter()
        .map(|chunk| {
            by_chunk_id
                .get(&chunk.manifest.chunk_id)
                .cloned()
                .ok_or_else(|| {
                    contract_error(
                        StopReason::InvalidSummarySchema,
                        format!(
                            "prompt_gc missing canonicalized summary for chunk_id '{}'",
                            chunk.manifest.chunk_id
                        ),
                    )
                })
        })
        .collect()
}

pub(crate) async fn build_runtime_plan(
    session: &Session,
    turn: &TurnContext,
    checkpoint_id: &str,
) -> Result<PromptGcRuntimePlan, crate::function_tool::FunctionCallError> {
    prompt_gc_checkpoint_for_id(session, turn, checkpoint_id).await?;
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
                payload_text: manifest_payload_text(&unit),
                call_name: match &unit.resolver {
                    PromptGcUnitResolver::Reasoning { .. } => None,
                    PromptGcUnitResolver::ToolPair { call_name, .. } => Some(call_name.clone()),
                    PromptGcUnitResolver::ToolResult { call_name, .. } => Some(call_name.clone()),
                },
            },
            exclusion_indices,
        });
    }
    Ok(PromptGcRuntimePlan { chunk_manifest })
}

pub(crate) async fn apply_runtime_plan(
    session: &Session,
    turn: &TurnContext,
    checkpoint: &PromptGcCheckpoint,
    plan: &PromptGcRuntimePlan,
    chunk_summaries: &[PromptGcChunkSummary],
) -> Result<PromptGcApplyOutcome, crate::function_tool::FunctionCallError> {
    validate_chunk_summaries(chunk_summaries, &plan.chunk_manifest)?;
    let ordered_chunk_summaries = order_chunk_summaries(&plan.chunk_manifest, chunk_summaries)?;
    let selected_chunks = select_chunks(plan, &ordered_chunk_summaries)?;
    let notes = build_notes(&ordered_chunk_summaries)?;
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
    let applied_unit_count = u64::try_from(applied_unit_keys.len()).unwrap_or(u64::MAX);

    let replacement_history = {
        let mut state = session.state.lock().await;
        let manage_context_checkpoint = state.manage_context_checkpoint();
        state.set_context_inclusion(&exclusion_indices, false);
        state.add_context_notes(notes);
        let replacement_history = state.prompt_snapshot_lenient();
        state.restore_manage_context_checkpoint(manage_context_checkpoint);
        replacement_history
    };

    session
        .persist_prompt_gc_replacement_history(
            turn,
            checkpoint,
            applied_unit_count,
            replacement_history,
        )
        .await
        .map_err(|error| {
            contract_error(
                StopReason::InvalidContract,
                format!("prompt_gc apply failed to persist replacement history: {error}"),
            )
        })?;

    if let Some(sidecar) = session.prompt_gc_sidecar_for_sub_id(&turn.sub_id).await {
        // Merge-safety anchor: committed prompt_gc rewrites must keep the
        // applied unit set recoverable until the cycle is finalized, or a
        // post-persist interruption can leave the sidecar and rollout out of sync.
        sidecar
            .lock()
            .await
            .note_apply_outcome(&checkpoint.checkpoint_id, applied_unit_keys.clone());
    }

    Ok(PromptGcApplyOutcome {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        checkpoint_seq: checkpoint.checkpoint_seq,
        applied_unit_keys,
    })
}

async fn prompt_gc_checkpoint_for_id(
    session: &Session,
    turn: &TurnContext,
    checkpoint_id: &str,
) -> Result<PromptGcCheckpoint, crate::function_tool::FunctionCallError> {
    let sidecar = session
        .prompt_gc_sidecar_for_sub_id(&turn.sub_id)
        .await
        .ok_or_else(|| {
            contract_error(
                StopReason::InvalidContract,
                "prompt_gc sidecar is not active for this turn",
            )
        })?;
    sidecar
        .lock()
        .await
        .checkpoint(checkpoint_id)
        .ok_or_else(|| {
            contract_error(
                StopReason::InvalidContract,
                format!("unknown checkpoint_id '{checkpoint_id}'"),
            )
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
            task: Arc::new(RegularTask) as Arc<dyn SessionTask>,
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
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");
        let chunk_id = plan.chunk_manifest[0].manifest.chunk_id.clone();

        let error = validate_chunk_summaries(
            &[
                PromptGcChunkSummary {
                    chunk_id: chunk_id.clone(),
                    tool_context: "tool".to_string(),
                    reasoning_context: "reasoning".to_string(),
                },
                PromptGcChunkSummary {
                    chunk_id,
                    tool_context: "tool".to_string(),
                    reasoning_context: "reasoning".to_string(),
                },
            ],
            &plan.chunk_manifest,
        )
        .expect_err("duplicate chunk ids must fail");

        let crate::function_tool::FunctionCallError::RespondToModel(message) = error else {
            panic!("expected model-visible contract error");
        };
        assert!(message.contains("invalid_summary_schema"));
        assert!(message.contains("appears more than once"));
    }

    #[tokio::test]
    async fn prompt_gc_apply_rejects_incomplete_chunk_summaries() {
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
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");
        assert_eq!(plan.chunk_manifest.len(), 2);
        let present_chunk_id = plan.chunk_manifest[0].manifest.chunk_id.clone();
        let missing_chunk_id = plan.chunk_manifest[1].manifest.chunk_id.clone();

        let error = validate_chunk_summaries(
            &[PromptGcChunkSummary {
                chunk_id: present_chunk_id,
                tool_context: "tool".to_string(),
                reasoning_context: "reasoning".to_string(),
            }],
            &plan.chunk_manifest,
        )
        .expect_err("missing chunk ids must fail");

        let crate::function_tool::FunctionCallError::RespondToModel(message) = error else {
            panic!("expected model-visible contract error");
        };
        assert!(message.contains("invalid_summary_schema"));
        assert!(message.contains("prompt_gc requires summaries for every chunk_manifest entry"));
        assert!(message.contains(&missing_chunk_id));
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
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
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
    async fn prompt_gc_runtime_limits_leave_later_checkpoint_eligible_units() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let mut initial_items = (0..(MAX_UNITS_PER_RETRIEVE + 1))
            .map(|index| ResponseItem::Reasoning {
                id: format!("reasoning-{index}"),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            })
            .collect::<Vec<_>>();
        initial_items.push(commentary_phase_message("phase-1"));
        session
            .record_conversation_items(turn_context.as_ref(), &initial_items)
            .await;

        let first_checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let first_plan = build_runtime_plan(&session, turn_context.as_ref(), &first_checkpoint_id)
            .await
            .expect("first retrieve plan");
        assert_eq!(first_plan.chunk_manifest.len(), MAX_UNITS_PER_RETRIEVE);
        let first_checkpoint =
            prompt_gc_checkpoint_for_id(&session, turn_context.as_ref(), &first_checkpoint_id)
                .await
                .expect("checkpoint");
        let outcome = apply_runtime_plan(
            &session,
            turn_context.as_ref(),
            &first_checkpoint,
            &first_plan,
            &first_plan
                .chunk_manifest
                .iter()
                .map(|chunk| PromptGcChunkSummary {
                    chunk_id: chunk.manifest.chunk_id.clone(),
                    tool_context: format!("tool {}", chunk.manifest.chunk_id),
                    reasoning_context: format!("reasoning {}", chunk.manifest.chunk_id),
                })
                .collect::<Vec<_>>(),
        )
        .await
        .expect("apply");
        sidecar
            .lock()
            .await
            .complete_cycle(crate::prompt_gc_sidecar::PromptGcApplyOutcome {
                checkpoint_id: first_checkpoint_id.clone(),
                checkpoint_seq: 0,
                applied_unit_keys: outcome.applied_unit_keys,
            });

        session
            .record_conversation_items(
                turn_context.as_ref(),
                &[commentary_phase_message("phase-2")],
            )
            .await;
        let second_checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let second_plan =
            build_runtime_plan(&session, turn_context.as_ref(), &second_checkpoint_id)
                .await
                .expect("second retrieve plan");

        assert_eq!(second_plan.chunk_manifest.len(), 1);
        assert_eq!(second_plan.chunk_manifest[0].manifest.kind, "reasoning");
        assert!(!session.clone_history().await.raw_items().is_empty());
    }

    #[tokio::test]
    async fn prompt_gc_rewrite_preserves_legacy_local_shell_id_only_pairs() {
        let (session, turn_context) = make_session_and_context().await;
        let turn_context = Arc::new(turn_context);
        let sidecar = install_prompt_gc_active_turn(&session, Arc::clone(&turn_context)).await;

        let mut items = (0..MAX_UNITS_PER_RETRIEVE)
            .map(|index| ResponseItem::Reasoning {
                id: format!("reasoning-{index}"),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            })
            .collect::<Vec<_>>();
        items.push(ResponseItem::LocalShellCall {
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
        });
        items.push(ResponseItem::FunctionCallOutput {
            call_id: "legacy-shell".to_string(),
            output: FunctionCallOutputPayload::from_text("/tmp".to_string()),
        });
        items.push(commentary_phase_message("phase-1"));
        session
            .record_conversation_items(turn_context.as_ref(), &items)
            .await;

        let checkpoint_id = activate_pending_checkpoint(&sidecar).await.checkpoint_id;
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");
        assert_eq!(plan.chunk_manifest.len(), MAX_UNITS_PER_RETRIEVE);
        let checkpoint =
            prompt_gc_checkpoint_for_id(&session, turn_context.as_ref(), &checkpoint_id)
                .await
                .expect("checkpoint");

        apply_runtime_plan(
            &session,
            turn_context.as_ref(),
            &checkpoint,
            &plan,
            &plan
                .chunk_manifest
                .iter()
                .map(|chunk| PromptGcChunkSummary {
                    chunk_id: chunk.manifest.chunk_id.clone(),
                    tool_context: String::new(),
                    reasoning_context: format!("reasoning {}", chunk.manifest.chunk_id),
                })
                .collect::<Vec<_>>(),
        )
        .await
        .expect("apply");

        let prompt = session.state.lock().await.prompt_snapshot_lenient();
        assert!(prompt.iter().any(|item| {
            matches!(
                item,
                ResponseItem::LocalShellCall {
                    id: Some(id),
                    call_id: None,
                    ..
                } if id == "legacy-shell"
            )
        }));
        assert!(prompt.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "legacy-shell"
            )
        }));
    }

    #[test]
    fn manifest_payload_text_truncates_oversize_units_for_manifest_preview() {
        let payload_text = "x".repeat(MAX_RAW_BYTES_PER_RETRIEVE + 10_000);
        let unit = PromptGcCapturedUnit {
            unit_key: 1,
            chunk_id: "chunk-1".to_string(),
            kind: PromptGcUnitKind::ToolPair,
            approx_bytes: payload_text.len(),
            function_call_output_token_qty: Some(10_000),
            payload_text,
            resolver: PromptGcUnitResolver::ToolPair {
                call_id: "call-1".to_string(),
                call_fingerprint: "call-fingerprint".to_string(),
                output_fingerprint: "output-fingerprint".to_string(),
                call_name: "shell".to_string(),
            },
        };

        let preview = manifest_payload_text(&unit);

        assert!(preview.contains("chars truncated"));
        assert!(preview.len() < unit.approx_bytes);
        assert!(preview.len() <= MAX_RAW_BYTES_PER_RETRIEVE + 64);
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
                namespace: None,
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
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
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
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");
        let checkpoint =
            prompt_gc_checkpoint_for_id(&session, turn_context.as_ref(), &checkpoint_id)
                .await
                .expect("checkpoint");

        apply_runtime_plan(
            &session,
            turn_context.as_ref(),
            &checkpoint,
            &plan,
            &plan
                .chunk_manifest
                .iter()
                .map(|chunk| PromptGcChunkSummary {
                    chunk_id: chunk.manifest.chunk_id.clone(),
                    tool_context: "same summary".to_string(),
                    reasoning_context: "same summary".to_string(),
                })
                .collect::<Vec<_>>(),
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

    #[tokio::test]
    async fn prompt_gc_apply_keeps_manifest_order_when_summaries_are_reordered() {
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
        let plan = build_runtime_plan(&session, turn_context.as_ref(), &checkpoint_id)
            .await
            .expect("retrieve plan");
        let checkpoint =
            prompt_gc_checkpoint_for_id(&session, turn_context.as_ref(), &checkpoint_id)
                .await
                .expect("checkpoint");

        let reversed_summaries = plan
            .chunk_manifest
            .iter()
            .rev()
            .map(|chunk| PromptGcChunkSummary {
                chunk_id: chunk.manifest.chunk_id.clone(),
                tool_context: format!("tool {}", chunk.manifest.chunk_id),
                reasoning_context: format!("reasoning {}", chunk.manifest.chunk_id),
            })
            .collect::<Vec<_>>();

        apply_runtime_plan(
            &session,
            turn_context.as_ref(),
            &checkpoint,
            &plan,
            &reversed_summaries,
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
        assert!(tool_notes[0].contains("chunk_id=prompt_gc_chunk_0"));
        assert!(tool_notes[0].contains("tool prompt_gc_chunk_0"));
        assert!(tool_notes[1].contains("chunk_id=prompt_gc_chunk_1"));
        assert!(tool_notes[1].contains("tool prompt_gc_chunk_1"));
    }
}
