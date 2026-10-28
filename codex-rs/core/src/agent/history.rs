//! Loads stored agent context and prepares the history a child inherits when it forks.

use crate::agent::types::SpawnAgentOptions;
use crate::config::Config;
use crate::context::BaseInstructionsFragment;
use crate::context::ContextualUserFragment;
use crate::context::CurrentTimeReminder;
use crate::context::CurrentTimeUnavailable;
use crate::context::GuardianApprovedAction;
use crate::context::ManagedDeveloperInstructions;
use crate::context::ModelInstructions;
use crate::context::MultiAgentModeInstructions;
use crate::context::MultiAgentRoleInstructions;
use crate::context::MultiAgentUsageHint;
use crate::context::world_state::IncrementalToolsHint;
use crate::context::world_state::PersistentModeState;
use crate::session::multi_agents::resolve_usage_hints_unchecked;
use crate::tasks::InterruptedTurnHistoryMarker;
use crate::thread_manager::ForkSnapshot;
use crate::thread_manager::ThreadManagerState;
use crate::thread_manager::fork_history_from_snapshot;
use codex_context_fragments::set_annotated_content;
use codex_context_fragments::to_annotated_content;
use codex_history::InitialHistory;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::ThreadId;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentUsageHintBinding;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use std::collections::HashSet;

fn keep_forked_rollout_item(item: &RolloutItem, preserve_context_baselines: bool) -> bool {
    match item {
        RolloutItem::ResponseItem(envelope) => match &envelope.item {
            ResponseItem::Message { role, phase, .. } => match role.as_str() {
                "system" | "developer" | "user" => true,
                "assistant" => matches!(
                    phase,
                    Some(MessagePhase::PartialAnswer | MessagePhase::FinalAnswer)
                ),
                _ => false,
            },
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::FunctionCallOutput { call_id: None, .. }
            | ResponseItem::ConfigurationUpdate { .. } => true,
            ResponseItem::AgentMessage { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::FunctionCallOutput {
                call_id: Some(_), ..
            }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => false,
        },
        RolloutItem::RealtimeItem(_)
        | RolloutItem::InterAgentCommunication(_)
        | RolloutItem::InterAgentCommunicationMetadata { .. }
        | RolloutItem::RetainedContext(_)
        | RolloutItem::SecurityRiskScore(_) => false,
        // Full-history forks preserve the cached prompt prefix and can keep diffing
        // from the parent's durable baseline unless legacy compaction requires rebuilding it.
        RolloutItem::TurnContext(_) | RolloutItem::WorldState(_) => preserve_context_baselines,
        // Child threads inherit model context, not the parent's cumulative usage state.
        RolloutItem::TokenUsageRecord(_) => false,
        RolloutItem::DynamicToolsUpdated { .. }
        | RolloutItem::Compacted(_)
        | RolloutItem::PostCompactRecoveryApplied(_)
        | RolloutItem::EventMsg(_)
        | RolloutItem::SessionMeta(_) => true,
    }
}

// Merge-safety anchor: fork sanitization keeps a recovery-application proof only when its
// (compaction_window_id, boundary_item_id) pair remains owned by retained history.
fn drop_unowned_recovery_applications(items: &mut Vec<RolloutItem>) {
    let owned_recovery_identities = items
        .iter()
        .filter_map(|item| {
            let RolloutItem::Compacted(compacted) = item else {
                return None;
            };
            let compaction_window_id = compacted.window_id.as_ref()?;
            let boundary_item_id = compacted
                .post_compact_recovery
                .as_ref()
                .map(|marker| marker.boundary_item_id.as_str())
                .or_else(|| {
                    compacted
                        .replacement_history
                        .as_ref()?
                        .last()?
                        .id()
                        .map(codex_protocol::ResponseItemId::as_str)
                })?;
            Some((compaction_window_id.clone(), boundary_item_id.to_string()))
        })
        .collect::<HashSet<_>>();
    items.retain(|item| match item {
        RolloutItem::PostCompactRecoveryApplied(applied) => owned_recovery_identities.contains(&(
            applied.compaction_window_id.clone(),
            applied.boundary_item_id.clone(),
        )),
        _ => true,
    });
}

// Merge-safety anchor: preserved V2 prefixes filter only usage hints against the captured birth binding; native non-identity context survives as inherited context, not child-local authorization.
fn retain_forked_developer_message(
    item: &mut ResponseItem,
    usage_hint_texts: &[String],
    retained_usage_hint_text: Option<&str>,
    preserve_fork_prefix: bool,
) -> bool {
    if !matches!(item, ResponseItem::Message { role, .. } if role == "developer") {
        return true;
    }

    let Some(mut content) = to_annotated_content(item) else {
        return false;
    };
    content.retain(|content_item| {
        // Leave base instructions for the later context-baseline retention check.
        if content_item.kind().as_str() == BaseInstructionsFragment::KIND {
            return true;
        }
        if !preserve_fork_prefix && content_item.kind().as_str() == GuardianApprovedAction::KIND {
            return false;
        }
        let ContentItem::InputText { text } = content_item.content() else {
            return true;
        };

        let is_retained_usage_hint = retained_usage_hint_text
            .is_some_and(|usage_hint_text| usage_hint_text == text.as_str());
        // Persisted role hints can predate the current bundled wording and lack markers.
        let is_usage_hint = matches!(
            content_item.kind().as_str(),
            MultiAgentRoleInstructions::KIND | MultiAgentUsageHint::KIND
        ) || MultiAgentRoleInstructions::matches_text(text)
            || usage_hint_texts
                .iter()
                .any(|usage_hint_text| usage_hint_text == text);
        if is_usage_hint && !is_retained_usage_hint {
            return false;
        }
        preserve_fork_prefix
            || !(text
                .starts_with(crate::guardian::AUTO_REVIEW_DENIED_ACTION_APPROVAL_DEVELOPER_PREFIX)
                || MultiAgentModeInstructions::matches_text(text)
                || CurrentTimeReminder::matches_text(text)
                || CurrentTimeUnavailable::matches_text(text))
    });
    !content.is_empty() && set_annotated_content(item, content).is_some()
}

pub(super) async fn load_agent_model_context(
    state: &ThreadManagerState,
    thread_id: ThreadId,
    history_mode: ThreadHistoryMode,
) -> CodexResult<Option<codex_thread_store::StoredModelContext>> {
    match history_mode {
        ThreadHistoryMode::Legacy => Ok(state
            .read_stored_thread(ReadThreadParams {
                thread_id,
                include_archived: true,
                include_history: true,
            })
            .await?
            .history
            .map(|history| codex_thread_store::StoredModelContext {
                thread_id,
                revision: history.revision,
                items: history.items,
            })),
        ThreadHistoryMode::Paginated => Ok(Some(
            state
                .load_latest_model_context(LoadThreadHistoryParams {
                    thread_id,
                    include_archived: true,
                })
                .await?,
        )),
    }
}

pub(super) struct PreparedForkedHistory {
    pub(super) initial_history: InitialHistory,
    pub(super) history_mode: Option<ThreadHistoryMode>,
    pub(super) dynamic_tools: Vec<DynamicToolSpec>,
}

/// Prepare copied history while leaving startup settings and timing with the caller.
pub(super) async fn prepare_forked_history(
    state: &ThreadManagerState,
    session_source: &SessionSource,
    options: &SpawnAgentOptions,
    config: &Config,
    multi_agent_version: MultiAgentVersion,
) -> CodexResult<PreparedForkedHistory> {
    if options.fork_parent_spawn_call_id.is_none() {
        return Err(CodexErr::Fatal(
            "spawn_agent fork requires a parent spawn call id".to_string(),
        ));
    }
    if options.fork_mode.is_none() {
        return Err(CodexErr::Fatal(
            "spawn_agent fork requires a fork mode".to_string(),
        ));
    }
    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id, ..
    }) = session_source
    else {
        return Err(CodexErr::Fatal(
            "spawn_agent fork requires a thread-spawn session source".to_string(),
        ));
    };

    let parent_thread_id = *parent_thread_id;
    let parent_thread = state.get_thread(parent_thread_id).await?;
    let parent_turn = if multi_agent_version == MultiAgentVersion::V2 {
        Some(parent_thread.session.new_default_turn().await)
    } else {
        None
    };
    let parent_developer_instructions = parent_turn.as_ref().and_then(|turn| {
        turn.developer_instructions
            .clone()
            .filter(|instructions| !instructions.is_empty())
    });
    let parent_history_mode = parent_thread.config_snapshot().await.history_mode;
    // `record_conversation_items` only queues persistence writes asynchronously.
    // Flush before snapshotting store history for a fork.
    parent_thread.ensure_rollout_materialized().await;
    parent_thread.flush_rollout().await?;

    // Merge-safety anchor: V2 forks sanitize SessionMeta but must retain the parent's history mode instead of the new-thread default.
    let destination_history_mode = Some(parent_history_mode);
    let mut forked_rollout_items =
        load_agent_model_context(state, parent_thread_id, parent_history_mode)
            .await?
            .ok_or_else(|| {
                CodexErr::Fatal(format!(
                    "parent thread history unavailable for fork: {parent_thread_id}"
                ))
            })?
            .items;

    // Merge-safety anchor: resolve the parent-owned catalog before removing its metadata and catalog control records; an empty capture must not fall back to ancestor history.
    let dynamic_tools = if multi_agent_version == MultiAgentVersion::V2 {
        codex_history::latest_dynamic_tools(&forked_rollout_items)
            .unwrap_or_default()
            .to_vec()
    } else {
        Vec::new()
    };

    let captured_usage_hint_text = match &config.agent_usage_hint_binding {
        AgentUsageHintBinding::Inherited {
            instructions: Some(instructions),
        } if multi_agent_version == MultiAgentVersion::V2 => {
            let ContentItem::InputText { text } =
                MultiAgentRoleInstructions::from_agent_usage_hint_instructions(
                    instructions.clone(),
                )
                .render()
            else {
                unreachable!("multi-agent role instructions render input text");
            };
            Some(text)
        }
        AgentUsageHintBinding::Resolve | AgentUsageHintBinding::Inherited { .. } => None,
    };
    let multi_agent_v2_usage_hint_texts_to_filter: Vec<String> =
        if multi_agent_version == MultiAgentVersion::V2 {
            let parent_config = parent_thread.session.get_config().await;
            let parent_usage_hints = resolve_usage_hints_unchecked(
                &parent_config.multi_agent_v2,
                ResolvedModelMessages::bundled().multi_agent(),
                !parent_config.update_plan_enabled,
            );
            [parent_usage_hints.root, parent_usage_hints.subagent]
                .into_iter()
                .flatten()
                .map(|instructions| {
                    let ContentItem::InputText { text } = instructions.render() else {
                        unreachable!("multi-agent role instructions render input text");
                    };
                    text
                })
                .chain(captured_usage_hint_text.iter().cloned())
                .collect()
        } else {
            Vec::new()
        };
    let mut preserve_context_baselines = true;
    for item in forked_rollout_items.iter().rev() {
        let RolloutItem::Compacted(compacted) = item else {
            continue;
        };
        // Legacy checkpoints force the child to rebuild context regardless of the
        // live parent's reference baseline; an older superseded checkpoint does not.
        if compacted.replacement_history.is_none() {
            preserve_context_baselines = false;
        }
        break;
    }
    // Merge-safety anchor: full-history V2 retains only its exact captured usage hint when
    // the reference baseline survives, including preserved-prefix forks. A reset leaves hint
    // reconstruction to the bound owner; it never resolves or appends a new child layer.
    let retained_usage_hint_text = if preserve_context_baselines {
        captured_usage_hint_text.as_deref()
    } else {
        None
    };
    let preserve_fork_prefix = multi_agent_version == MultiAgentVersion::V2
        && config.multi_agent_v2.preserve_fork_prefix
        && preserve_context_baselines;
    if preserve_fork_prefix {
        let InitialHistory::Forked(items) = fork_history_from_snapshot(
            ForkSnapshot::Interrupted,
            InitialHistory::Forked(forked_rollout_items),
            InterruptedTurnHistoryMarker::Disabled,
        ) else {
            unreachable!("a forked snapshot remains forked");
        };
        forked_rollout_items = items;
    }
    // Apply the narrow captured-hint exception to standalone and compacted prefix content; ordinary forks still sanitize non-identity context before rebuilding the child's view.
    let retain_forked_item = |envelope: &mut ResponseItemEnvelope| {
        if multi_agent_version == MultiAgentVersion::V2
            && (preserve_fork_prefix
                || matches!(&envelope.item, ResponseItem::Message { role, .. } if role == "user" || role == "assistant"))
        {
            // Resume must not recapture copied parent messages as local authorization.
            envelope
                .metadata
                .get_or_insert_default()
                .inherited_user_message = true;
        }
        if let Some(metadata) = &mut envelope.metadata
            && (metadata.sender_user_messages.take().is_some()
                || !matches!(&envelope.item, ResponseItem::Message { role, .. } if role == "user"))
        {
            // Assistant and tool positions belong to the parent's counter, not the child.
            metadata.user_input_order = None;
        }
        let response_item = &mut envelope.item;
        if !retain_forked_developer_message(
            response_item,
            &multi_agent_v2_usage_hint_texts_to_filter,
            retained_usage_hint_text,
            preserve_fork_prefix,
        ) {
            return false;
        }
        if preserve_fork_prefix {
            return true;
        }
        // Tool declarations and their comparison baseline must survive or be rebuilt together.
        // Apply this to both standalone items and compaction replacement histories.
        if matches!(response_item, ResponseItem::AdditionalTools { .. }) {
            return preserve_context_baselines;
        }
        if matches!(response_item, ResponseItem::AgentMessage { .. }) {
            return false;
        }

        if matches!(response_item, ResponseItem::Message { role, .. } if role == "developer") {
            let Some(mut content) = to_annotated_content(response_item) else {
                return false;
            };
            content.retain_mut(|content_item| {
                if matches!(
                    content_item.kind().as_str(),
                    BaseInstructionsFragment::KIND | IncrementalToolsHint::KIND
                ) {
                    return preserve_context_baselines;
                }
                let ContentItem::InputText { text } = content_item.content_mut() else {
                    return true;
                };
                if ManagedDeveloperInstructions::matches_text(text)
                    || ModelInstructions::matches_text(text)
                    || PersistentModeState::matches_text(text)
                {
                    // If the child will rebuild its initial context, drop the inherited
                    // instructions; startup will add the current requirements and effort
                    // instructions once.
                    return preserve_context_baselines;
                }
                if preserve_context_baselines {
                    return true;
                }
                let Some(parent_developer_instructions) = parent_developer_instructions.as_ref()
                else {
                    return true;
                };
                // TODO(anp) track better message fragment provenance in rollouts.
                if !text.contains(parent_developer_instructions) {
                    return true;
                }

                *text = text.replace(parent_developer_instructions, "");
                !text.is_empty()
            });
            return !content.is_empty() && set_annotated_content(response_item, content).is_some();
        }

        true
    };
    forked_rollout_items.retain_mut(|item| {
        let keep = (preserve_fork_prefix && matches!(item, RolloutItem::ResponseItem(_)))
            || keep_forked_rollout_item(item, preserve_context_baselines);
        // Merge-safety anchor: a V2 child persists its own birth binding, never the copied
        // parent's SessionMeta, even when native history retains the full response prefix.
        if !keep
            || multi_agent_version == MultiAgentVersion::V2
                && matches!(
                    &*item,
                    RolloutItem::SessionMeta(_) | RolloutItem::DynamicToolsUpdated { .. }
                )
            || destination_history_mode == Some(ThreadHistoryMode::Paginated)
                && matches!(
                    &*item,
                    RolloutItem::EventMsg(
                        EventMsg::ItemCompleted(_)
                            | EventMsg::TokenCount(_)
                            | EventMsg::ThreadGoalUpdated(_)
                            | EventMsg::ThreadSettingsApplied(_),
                    )
                )
        {
            return false;
        }

        match item {
            RolloutItem::ResponseItem(response_item) => retain_forked_item(response_item),
            RolloutItem::Compacted(compacted) => {
                // This compaction becomes part of the subagent's initial history. Rewrite its
                // metadata to describe the child rather than the parent.
                compacted.latest_token_usage_record = None;
                if let Some(resume_metadata) = &mut compacted.resume_metadata {
                    if multi_agent_version == MultiAgentVersion::V2 {
                        resume_metadata.dynamic_tools = None;
                    }
                    resume_metadata.multi_agent_version = Some(multi_agent_version);
                    resume_metadata.turn_attribution = None;
                    if !preserve_context_baselines {
                        resume_metadata.previous_turn_settings = None;
                    }
                }
                // Parent-local review evidence must not become the child's authorization.
                // Root user authorization is collected separately by the host.
                compacted.guardian_history = None;
                // Only V2 fetches root authorization live. Its local scope starts known-empty;
                // V1 must remain incomplete when inherited authorization has been stripped.
                compacted.retained_context = (multi_agent_version == MultiAgentVersion::V2)
                    .then(codex_history::RetainedContext::default);
                if let Some(replacement_history) = compacted.replacement_history.as_mut() {
                    replacement_history.retain_mut(&retain_forked_item);
                }
                true
            }
            RolloutItem::WorldState(_) => true,
            RolloutItem::RealtimeItem(_) => false,
            RolloutItem::DynamicToolsUpdated { .. }
            | RolloutItem::EventMsg(_)
            | RolloutItem::SessionMeta(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::InterAgentCommunication(_)
            | RolloutItem::InterAgentCommunicationMetadata { .. }
            | RolloutItem::PostCompactRecoveryApplied(_) => true,
            RolloutItem::RetainedContext(_)
            | RolloutItem::TokenUsageRecord(_)
            | RolloutItem::SecurityRiskScore(_) => false,
        }
    });
    if !preserve_context_baselines {
        drop_unowned_recovery_applications(&mut forked_rollout_items);
    }
    Ok(PreparedForkedHistory {
        initial_history: InitialHistory::Forked(forked_rollout_items),
        history_mode: destination_history_mode,
        dynamic_tools,
    })
}
