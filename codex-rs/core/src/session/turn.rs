use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crate::SkillInjections;
use crate::SkillLoadOutcome;
use crate::build_skill_injections;
use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::collect_env_var_dependencies;
use crate::collect_explicit_skill_mentions;
use crate::compact::InitialContextInjection;
use crate::compact::collect_user_messages;
use crate::compact::run_inline_auto_compact_task;
use crate::compact::should_use_remote_compact_task;
use crate::compact_remote::run_inline_remote_auto_compact_task;
use crate::connectors;
use crate::feedback_tags;
use crate::hook_runtime::PendingInputHookDisposition;
use crate::hook_runtime::emit_hook_completed_events;
use crate::hook_runtime::inspect_pending_input;
use crate::hook_runtime::record_additional_contexts;
use crate::hook_runtime::record_pending_input;
use crate::hook_runtime::run_pending_session_start_hooks;
use crate::hook_runtime::run_user_prompt_submit_hooks;
use crate::injection::ToolMentionKind;
use crate::injection::app_id_from_path;
use crate::injection::tool_kind_for_path;
use crate::mcp_skill_dependencies::maybe_prompt_and_install_mcp_dependencies;
use crate::mcp_tool_exposure::build_mcp_tool_exposure;
use crate::mentions::build_connector_slug_counts;
use crate::mentions::build_skill_name_counts;
use crate::mentions::collect_explicit_app_ids;
use crate::mentions::collect_explicit_plugin_mentions;
use crate::mentions::collect_tool_mentions_from_messages;
use crate::parse_turn_item;
use crate::plugins::build_plugin_injections;
use crate::resolve_skill_dependencies_for_turn;
use crate::session::PreviousTurnSettings;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::stream_events_utils::HandleOutputCtx;
use crate::stream_events_utils::SamplingExecutionMode;
use crate::stream_events_utils::handle_non_tool_response_item;
use crate::stream_events_utils::handle_output_item_done;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::stream_events_utils::mark_thread_memory_mode_polluted_if_external_context;
use crate::stream_events_utils::raw_assistant_output_text_from_item;
use crate::stream_events_utils::record_completed_response_item;
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolCallSource;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::router::ToolRouterParams;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::turn_timing::record_turn_ttft_metric;
use crate::unavailable_tool::collect_unavailable_called_tools;
use crate::util::backoff;
use crate::util::error_or_panic;
use chrono::DateTime;
use chrono::Utc;
use codex_analytics::AppInvocation;
use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_analytics::InvocationType;
use codex_analytics::TurnResolvedConfigFact;
use codex_analytics::build_track_events_context;
use codex_async_utils::OrCancelExt;
use codex_features::Feature;
use codex_hooks::HookEvent;
use codex_hooks::HookEventAfterAgent;
use codex_hooks::HookPayload;
use codex_hooks::HookResult;
use codex_login::AccountRateLimitRefreshOutcome;
use codex_login::AccountRateLimitRefreshRoster;
use codex_login::AccountRateLimitRefreshRosterStatus;
use codex_login::ChatGptRequestAuth;
use codex_login::ChatgptAccountAuthResolution;
use codex_login::ChatgptAccountRefreshMode;
use codex_login::UsageLimitAutoSwitchFallbackSelectionMode;
use codex_login::UsageLimitAutoSwitchRequest;
use codex_login::UsageLimitAutoSwitchSelectionScope;
use codex_protocol::config_types::ModeKind;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::items::PlanItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AgentReasoningSectionBreakEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::PROMPT_GC_COMPACTION_MESSAGE;
use codex_protocol::protocol::PlanDeltaEvent;
use codex_protocol::protocol::PromptGcCompactionMetadata;
use codex_protocol::protocol::PromptGcExecutionPhase;
use codex_protocol::protocol::PromptGcOutcomeKind;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::ReasoningContentDeltaEvent;
use codex_protocol::protocol::ReasoningRawContentDeltaEvent;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TurnDiffEvent;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use codex_tools::ToolName;
use codex_tools::filter_tool_suggest_discoverable_tools_for_client;
use codex_utils_stream_parser::AssistantTextChunk;
use codex_utils_stream_parser::AssistantTextStreamParser;
use codex_utils_stream_parser::ProposedPlanSegment;
use codex_utils_stream_parser::extract_proposed_plan_text;
use codex_utils_stream_parser::strip_citations;
use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::field;
use tracing::info;
use tracing::instrument;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;

/// Takes a user message as input and runs a loop where, at each sampling request, the model
/// replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single sampling request, in practice, we generally one item per sampling request:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next sampling request.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the turn complete.
///
pub(crate) async fn run_turn(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
    prewarmed_client_session: Option<ModelClientSession>,
    cancellation_token: CancellationToken,
) -> Option<String> {
    if input.is_empty() && !sess.has_pending_input().await {
        return None;
    }

    let model_info = turn_context.model_info.clone();
    let auto_compact_limit = model_info.auto_compact_token_limit().unwrap_or(i64::MAX);
    let mut prewarmed_client_session = prewarmed_client_session;
    // TODO(ccunningham): Pre-turn compaction runs before context updates and the
    // new user message are recorded. Estimate pending incoming items (context
    // diffs/full reinjection + user input) and trigger compaction preemptively
    // when they would push the thread over the compaction threshold.
    let pre_sampling_compacted = match run_pre_sampling_compact(&sess, &turn_context).await {
        Ok(pre_sampling_compacted) => pre_sampling_compacted,
        Err(_) => {
            error!("Failed to run pre-sampling compact");
            return None;
        }
    };
    if pre_sampling_compacted && let Some(mut client_session) = prewarmed_client_session.take() {
        client_session.reset_websocket_session();
    }

    let skills_outcome = Some(turn_context.turn_skills.outcome.as_ref());

    if let Err(error) = sess
        .record_context_updates_and_set_reference_context_item(turn_context.as_ref())
        .await
    {
        info!("Turn context error: {error:#}");
        let event = EventMsg::Error(error.to_error_event(/*message_prefix*/ None));
        sess.send_event(&turn_context, event).await;
        return None;
    }

    let loaded_plugins = sess
        .services
        .plugins_manager
        .plugins_for_config(&turn_context.config)
        .await;
    // Structured plugin:// mentions are resolved from the current session's
    // enabled plugins, then converted into turn-scoped guidance below.
    let mentioned_plugins =
        collect_explicit_plugin_mentions(&input, loaded_plugins.capability_summaries());
    let apps_enabled = match turn_context.apps_enabled() {
        Ok(apps_enabled) => apps_enabled,
        Err(error) => {
            info!("Turn app-auth error: {error:#}");
            let event = EventMsg::Error(error.to_error_event(/*message_prefix*/ None));
            sess.send_event(&turn_context, event).await;
            return None;
        }
    };
    let mcp_tools = if apps_enabled || !mentioned_plugins.is_empty() {
        // Plugin mentions need raw MCP/app inventory even when app tools
        // are normally hidden so we can describe the plugin's currently
        // usable capabilities for this turn.
        match sess
            .services
            .mcp_connection_manager
            .read()
            .await
            .list_all_tools()
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(mcp_tools) => mcp_tools,
            Err(_) if apps_enabled => return None,
            Err(_) => HashMap::new(),
        }
    } else {
        HashMap::new()
    };
    let available_connectors = if apps_enabled {
        let connectors = codex_connectors::merge::merge_plugin_connectors_with_accessible(
            loaded_plugins
                .effective_apps()
                .into_iter()
                .map(|connector_id| connector_id.0),
            connectors::accessible_connectors_from_mcp_tools(&mcp_tools),
        );
        connectors::with_app_enabled_state(connectors, &turn_context.config)
    } else {
        Vec::new()
    };
    let connector_slug_counts = build_connector_slug_counts(&available_connectors);
    let skill_name_counts_lower = skills_outcome
        .as_ref()
        .map_or_else(HashMap::new, |outcome| {
            build_skill_name_counts(&outcome.skills, &outcome.disabled_paths).1
        });
    let mentioned_skills = skills_outcome.as_ref().map_or_else(Vec::new, |outcome| {
        collect_explicit_skill_mentions(
            &input,
            &outcome.skills,
            &outcome.disabled_paths,
            &connector_slug_counts,
        )
    });
    let config = turn_context.config.clone();
    if config
        .features
        .enabled(Feature::SkillEnvVarDependencyPrompt)
    {
        let env_var_dependencies = collect_env_var_dependencies(&mentioned_skills);
        resolve_skill_dependencies_for_turn(&sess, &turn_context, &env_var_dependencies).await;
    }

    maybe_prompt_and_install_mcp_dependencies(
        sess.as_ref(),
        turn_context.as_ref(),
        &cancellation_token,
        &mentioned_skills,
    )
    .await;

    let session_telemetry = turn_context.session_telemetry.clone();
    let thread_id = sess.conversation_id.to_string();
    let tracking = build_track_events_context(
        turn_context.model_info.slug.clone(),
        thread_id,
        turn_context.sub_id.clone(),
    );
    let SkillInjections {
        items: skill_items,
        warnings: skill_warnings,
    } = build_skill_injections(
        &mentioned_skills,
        skills_outcome,
        Some(&session_telemetry),
        &sess.services.analytics_events_client,
        tracking.clone(),
    )
    .await;

    for message in skill_warnings {
        sess.send_event(&turn_context, EventMsg::Warning(WarningEvent { message }))
            .await;
    }

    let plugin_items =
        build_plugin_injections(&mentioned_plugins, &mcp_tools, &available_connectors);
    let mentioned_plugin_metadata = mentioned_plugins
        .iter()
        .filter_map(crate::plugins::PluginCapabilitySummary::telemetry_metadata)
        .collect::<Vec<_>>();

    let mut explicitly_enabled_connectors = collect_explicit_app_ids(&input);
    explicitly_enabled_connectors.extend(collect_explicit_app_ids_from_skill_items(
        &skill_items,
        &available_connectors,
        &skill_name_counts_lower,
    ));
    let connector_names_by_id = available_connectors
        .iter()
        .map(|connector| (connector.id.as_str(), connector.name.as_str()))
        .collect::<HashMap<&str, &str>>();
    let mentioned_app_invocations = explicitly_enabled_connectors
        .iter()
        .map(|connector_id| AppInvocation {
            connector_id: Some(connector_id.clone()),
            app_name: connector_names_by_id
                .get(connector_id.as_str())
                .map(|name| (*name).to_string()),
            invocation_type: Some(InvocationType::Explicit),
        })
        .collect::<Vec<_>>();

    if run_pending_session_start_hooks(&sess, &turn_context).await {
        return None;
    }
    let additional_contexts = if input.is_empty() {
        Vec::new()
    } else {
        let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input.clone());
        let response_item: ResponseItem = initial_input_for_turn.clone().into();
        let user_prompt_submit_outcome = run_user_prompt_submit_hooks(
            &sess,
            &turn_context,
            UserMessageItem::new(&input).message(),
        )
        .await;
        if user_prompt_submit_outcome.should_stop {
            record_additional_contexts(
                &sess,
                &turn_context,
                user_prompt_submit_outcome.additional_contexts,
            )
            .await;
            return None;
        }
        sess.record_user_prompt_and_emit_turn_item(turn_context.as_ref(), &input, response_item)
            .await;
        user_prompt_submit_outcome.additional_contexts
    };
    sess.services
        .analytics_events_client
        .track_app_mentioned(tracking.clone(), mentioned_app_invocations);
    for plugin in mentioned_plugin_metadata {
        sess.services
            .analytics_events_client
            .track_plugin_used(tracking.clone(), plugin);
    }
    sess.merge_connector_selection(explicitly_enabled_connectors.clone())
        .await;
    record_additional_contexts(&sess, &turn_context, additional_contexts).await;
    if !input.is_empty() {
        // Track the previous-turn baseline from the regular user-turn path only so
        // standalone tasks (compact/shell/review/undo) cannot suppress future
        // model/realtime injections.
        sess.set_previous_turn_settings(Some(PreviousTurnSettings {
            model: turn_context.model_info.slug.clone(),
            realtime_active: Some(turn_context.realtime_active),
        }))
        .await;
    }
    let agent_task = match sess.ensure_agent_task_registered().await {
        Ok(agent_task) => agent_task,
        Err(error) => {
            warn!(error = %error, "agent task registration failed");
            sess.send_event(
                turn_context.as_ref(),
                EventMsg::Error(ErrorEvent {
                    message: format!(
                        "Agent task registration failed. Please try again; Codex will attempt to register the task again on the next turn: {error}"
                    ),
                    codex_error_info: Some(CodexErrorInfo::Other),
                }),
            )
            .await;
            return None;
        }
    };

    if !skill_items.is_empty() {
        sess.record_conversation_items(&turn_context, &skill_items)
            .await;
    }
    if !plugin_items.is_empty() {
        sess.record_conversation_items(&turn_context, &plugin_items)
            .await;
    }

    track_turn_resolved_config_analytics(&sess, &turn_context, &input).await;

    let skills_outcome = Some(turn_context.turn_skills.outcome.as_ref());
    sess.maybe_start_ghost_snapshot(Arc::clone(&turn_context), cancellation_token.child_token())
        .await;
    let mut last_agent_message: Option<String> = None;
    let mut stop_hook_active = false;
    // Although from the perspective of codex.rs, TurnDiffTracker has the lifecycle of a Task which contains
    // many turns, from the perspective of the user, it is a single turn.
    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let mut server_model_warning_emitted_for_turn = false;

    // `ModelClientSession` is turn-scoped and caches WebSocket + sticky routing state, so we reuse
    // one instance across retries within this turn.
    let mut prewarmed_client_session = prewarmed_client_session;
    if agent_task.is_some()
        && let Some(prewarmed_client_session) = prewarmed_client_session.as_mut()
    {
        prewarmed_client_session.disable_cached_websocket_session_on_drop();
    }
    let mut client_session = if let Some(agent_task) = agent_task {
        sess.services
            .model_client
            .new_session_with_agent_task(Some(agent_task))
    } else if let Some(prewarmed_client_session) = prewarmed_client_session.take() {
        prewarmed_client_session
    } else {
        sess.services.model_client.new_session()
    };
    // Pending input is drained into history before building the next model request.
    // However, we defer that drain until after sampling in two cases:
    // 1. At the start of a turn, so the fresh user prompt in `input` gets sampled first.
    // 2. After auto-compact, when model/tool continuation needs to resume before any steer.
    let mut can_drain_pending_input = input.is_empty();

    loop {
        if run_pending_session_start_hooks(&sess, &turn_context).await {
            break;
        }

        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_input = if can_drain_pending_input {
            sess.get_pending_input().await
        } else {
            Vec::new()
        };

        let mut blocked_pending_input = false;
        let mut blocked_pending_input_contexts = Vec::new();
        let mut requeued_pending_input = false;
        let mut accepted_pending_input = Vec::new();
        if !pending_input.is_empty() {
            let mut pending_input_iter = pending_input.into_iter();
            while let Some(pending_input_item) = pending_input_iter.next() {
                match inspect_pending_input(&sess, &turn_context, pending_input_item).await {
                    PendingInputHookDisposition::Accepted(pending_input) => {
                        accepted_pending_input.push(*pending_input);
                    }
                    PendingInputHookDisposition::Blocked {
                        additional_contexts,
                    } => {
                        let remaining_pending_input = pending_input_iter.collect::<Vec<_>>();
                        if !remaining_pending_input.is_empty() {
                            let _ = sess.prepend_pending_input(remaining_pending_input).await;
                            requeued_pending_input = true;
                        }
                        blocked_pending_input_contexts = additional_contexts;
                        blocked_pending_input = true;
                        break;
                    }
                }
            }
        }

        let has_accepted_pending_input = !accepted_pending_input.is_empty();
        for pending_input in accepted_pending_input {
            record_pending_input(&sess, &turn_context, pending_input).await;
        }
        record_additional_contexts(&sess, &turn_context, blocked_pending_input_contexts).await;

        if blocked_pending_input && !has_accepted_pending_input {
            if requeued_pending_input {
                continue;
            }
            break;
        }

        // Construct the input that we will send to the model.
        let sampling_request_input: Vec<ResponseItem> = {
            sess.clone_history()
                .await
                .for_prompt(&turn_context.model_info.input_modalities)
        };

        let sampling_request_input_messages = sampling_request_input
            .iter()
            .filter_map(|item| match parse_turn_item(item) {
                Some(TurnItem::UserMessage(user_message)) => Some(user_message),
                _ => None,
            })
            .map(|user_message| user_message.message())
            .collect::<Vec<String>>();
        let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
        match run_sampling_request(
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            Arc::clone(&turn_diff_tracker),
            &mut client_session,
            turn_metadata_header.as_deref(),
            sampling_request_input,
            &explicitly_enabled_connectors,
            skills_outcome,
            None,
            &mut server_model_warning_emitted_for_turn,
            cancellation_token.child_token(),
        )
        .await
        {
            Ok(sampling_request_output) => {
                let SamplingRequestResult {
                    needs_follow_up: model_needs_follow_up,
                    last_agent_message: sampling_request_last_agent_message,
                    ..
                } = sampling_request_output;
                // Merge-safety anchor: PromptGcSidecar runs after the main request has fully
                // drained tool futures, but before token-pressure and auto-compact checks. That
                // keeps phase-checkpoint compaction on the same canonical history the next
                // prompt snapshot will read.
                run_prompt_gc_sidecar_if_needed(
                    &sess,
                    &turn_context,
                    Arc::clone(&turn_diff_tracker),
                    &client_session,
                    cancellation_token.child_token(),
                )
                .await;
                can_drain_pending_input = true;
                let has_pending_input = sess.has_pending_input().await;
                let needs_follow_up = model_needs_follow_up || has_pending_input;
                let total_usage_tokens = sess.get_total_token_usage().await;
                let token_limit_reached = total_usage_tokens >= auto_compact_limit;

                let estimated_token_count =
                    sess.get_estimated_token_count(turn_context.as_ref()).await;

                trace!(
                    turn_id = %turn_context.sub_id,
                    total_usage_tokens,
                    estimated_token_count = ?estimated_token_count,
                    auto_compact_limit,
                    token_limit_reached,
                    model_needs_follow_up,
                    has_pending_input,
                    needs_follow_up,
                    "post sampling token usage"
                );

                // as long as compaction works well in getting us way below the token limit, we shouldn't worry about being in an infinite loop.
                if token_limit_reached && needs_follow_up {
                    if run_auto_compact(
                        &sess,
                        &turn_context,
                        InitialContextInjection::BeforeLastUserMessage,
                        CompactionReason::ContextLimit,
                        CompactionPhase::MidTurn,
                    )
                    .await
                    .is_err()
                    {
                        return None;
                    }
                    client_session.reset_websocket_session();
                    can_drain_pending_input = !model_needs_follow_up;
                    continue;
                }

                if !needs_follow_up {
                    last_agent_message = sampling_request_last_agent_message;
                    let stop_hook_permission_mode = match turn_context.approval_policy.value() {
                        AskForApproval::Never => "bypassPermissions",
                        AskForApproval::UnlessTrusted
                        | AskForApproval::OnFailure
                        | AskForApproval::OnRequest
                        | AskForApproval::Granular(_) => "default",
                    }
                    .to_string();
                    let stop_request = codex_hooks::StopRequest {
                        session_id: sess.conversation_id,
                        turn_id: turn_context.sub_id.clone(),
                        cwd: turn_context.cwd.clone(),
                        transcript_path: sess.hook_transcript_path().await,
                        model: turn_context.model_info.slug.clone(),
                        permission_mode: stop_hook_permission_mode,
                        stop_hook_active,
                        last_assistant_message: last_agent_message.clone(),
                    };
                    for run in sess.hooks().preview_stop(&stop_request) {
                        sess.send_event(
                            &turn_context,
                            EventMsg::HookStarted(codex_protocol::protocol::HookStartedEvent {
                                turn_id: Some(turn_context.sub_id.clone()),
                                run,
                            }),
                        )
                        .await;
                    }
                    let stop_outcome = sess.hooks().run_stop(stop_request).await;
                    emit_hook_completed_events(&sess, &turn_context, stop_outcome.hook_events)
                        .await;
                    if stop_outcome.should_block {
                        if let Some(hook_prompt_message) =
                            build_hook_prompt_message(&stop_outcome.continuation_fragments)
                        {
                            sess.record_conversation_items(
                                &turn_context,
                                std::slice::from_ref(&hook_prompt_message),
                            )
                            .await;
                            stop_hook_active = true;
                            continue;
                        } else {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Stop hook requested continuation without a prompt; ignoring the block.".to_string(),
                                }),
                            )
                            .await;
                        }
                    }
                    if stop_outcome.should_stop {
                        break;
                    }
                    let hook_outcomes = sess
                        .hooks()
                        .dispatch(HookPayload {
                            session_id: sess.conversation_id,
                            cwd: turn_context.cwd.clone(),
                            client: turn_context.app_server_client_name.clone(),
                            triggered_at: chrono::Utc::now(),
                            hook_event: HookEvent::AfterAgent {
                                event: HookEventAfterAgent {
                                    thread_id: sess.conversation_id,
                                    turn_id: turn_context.sub_id.clone(),
                                    input_messages: sampling_request_input_messages,
                                    last_assistant_message: last_agent_message.clone(),
                                },
                            },
                        })
                        .await;

                    let mut abort_message = None;
                    for hook_outcome in hook_outcomes {
                        let hook_name = hook_outcome.hook_name;
                        match hook_outcome.result {
                            HookResult::Success => {}
                            HookResult::FailedContinue(error) => {
                                warn!(
                                    turn_id = %turn_context.sub_id,
                                    hook_name = %hook_name,
                                    error = %error,
                                    "after_agent hook failed; continuing"
                                );
                            }
                            HookResult::FailedAbort(error) => {
                                let message = format!(
                                    "after_agent hook '{hook_name}' failed and aborted turn completion: {error}"
                                );
                                warn!(
                                    turn_id = %turn_context.sub_id,
                                    hook_name = %hook_name,
                                    error = %error,
                                    "after_agent hook failed; aborting operation"
                                );
                                if abort_message.is_none() {
                                    abort_message = Some(message);
                                }
                            }
                        }
                    }
                    if let Some(message) = abort_message {
                        sess.send_event(
                            &turn_context,
                            EventMsg::Error(ErrorEvent {
                                message,
                                codex_error_info: None,
                            }),
                        )
                        .await;
                        return None;
                    }
                    if let Err(err) = sess
                        .clear_pending_post_compact_recovery_after_successful_turn()
                        .await
                    {
                        sess.send_event(
                            &turn_context,
                            EventMsg::Error(err.to_error_event(Some(
                                "Error clearing post-compact recovery state".to_string(),
                            ))),
                        )
                        .await;
                        return None;
                    }
                    break;
                }
                continue;
            }
            Err(CodexErr::TurnAborted) => {
                // Aborted turn is reported via a different event.
                break;
            }
            Err(CodexErr::InvalidImageRequest()) => {
                {
                    let mut state = sess.state.lock().await;
                    error_or_panic(
                        "Invalid image detected; sanitizing tool output to prevent poisoning",
                    );
                    if state.history.replace_last_turn_images("Invalid image") {
                        continue;
                    }
                }

                let event = EventMsg::Error(ErrorEvent {
                    message: "Invalid image in your last message. Please remove it and try again."
                        .to_string(),
                    codex_error_info: Some(CodexErrorInfo::BadRequest),
                });
                sess.send_event(&turn_context, event).await;
                break;
            }
            Err(e) => {
                info!("Turn error: {e:#}");
                let event = EventMsg::Error(e.to_error_event(/*message_prefix*/ None));
                sess.send_event(&turn_context, event).await;
                // let the user continue the conversation
                break;
            }
        }
    }

    last_agent_message
}

async fn track_turn_resolved_config_analytics(
    sess: &Session,
    turn_context: &TurnContext,
    input: &[UserInput],
) {
    if !sess.enabled(Feature::GeneralAnalytics) {
        return;
    }

    let thread_config = {
        let state = sess.state.lock().await;
        state.session_configuration.thread_config_snapshot()
    };
    let is_first_turn = {
        let mut state = sess.state.lock().await;
        state.take_next_turn_is_first()
    };
    sess.services
        .analytics_events_client
        .track_turn_resolved_config(TurnResolvedConfigFact {
            turn_id: turn_context.sub_id.clone(),
            thread_id: sess.conversation_id.to_string(),
            num_input_images: input
                .iter()
                .filter(|item| {
                    matches!(item, UserInput::Image { .. } | UserInput::LocalImage { .. })
                })
                .count(),
            submission_type: None,
            ephemeral: thread_config.ephemeral,
            session_source: thread_config.session_source,
            model: turn_context.model_info.slug.clone(),
            model_provider: turn_context.config.model_provider_id.clone(),
            sandbox_policy: turn_context.sandbox_policy.get().clone(),
            reasoning_effort: turn_context.reasoning_effort,
            reasoning_summary: Some(turn_context.reasoning_summary),
            service_tier: turn_context.config.service_tier,
            approval_policy: turn_context.approval_policy.value(),
            approvals_reviewer: turn_context.config.approvals_reviewer,
            sandbox_network_access: turn_context.network_sandbox_policy.is_enabled(),
            collaboration_mode: turn_context.collaboration_mode.mode,
            personality: turn_context.personality,
            is_first_turn,
        });
}

async fn run_pre_sampling_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> CodexResult<bool> {
    let total_usage_tokens_before_compaction = sess.get_total_token_usage().await;
    let mut pre_sampling_compacted = maybe_run_previous_model_inline_compact(
        sess,
        turn_context,
        total_usage_tokens_before_compaction,
    )
    .await?;
    let total_usage_tokens = sess.get_total_token_usage().await;
    let auto_compact_limit = turn_context
        .model_info
        .auto_compact_token_limit()
        .unwrap_or(i64::MAX);
    // Compact if the total usage tokens are greater than the auto compact limit
    if total_usage_tokens >= auto_compact_limit {
        run_auto_compact(
            sess,
            turn_context,
            InitialContextInjection::DoNotInject,
            CompactionReason::ContextLimit,
            CompactionPhase::PreTurn,
        )
        .await?;
        pre_sampling_compacted = true;
    }
    Ok(pre_sampling_compacted)
}

/// Runs pre-sampling compaction against the previous model when switching to a smaller
/// context-window model.
///
/// Returns `Ok(true)` when compaction ran successfully, `Ok(false)` when compaction was skipped
/// because the model/context-window preconditions were not met, and `Err(_)` only when compaction
/// was attempted and failed.
async fn maybe_run_previous_model_inline_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    total_usage_tokens: i64,
) -> CodexResult<bool> {
    let Some(previous_turn_settings) = sess.previous_turn_settings().await else {
        return Ok(false);
    };
    let previous_model_turn_context = Arc::new(
        turn_context
            .with_model(previous_turn_settings.model, &sess.services.models_manager)
            .await?,
    );

    let Some(old_context_window) = previous_model_turn_context.model_context_window() else {
        return Ok(false);
    };
    let Some(new_context_window) = turn_context.model_context_window() else {
        return Ok(false);
    };
    let new_auto_compact_limit = turn_context
        .model_info
        .auto_compact_token_limit()
        .unwrap_or(i64::MAX);
    let should_run = total_usage_tokens > new_auto_compact_limit
        && previous_model_turn_context.model_info.slug != turn_context.model_info.slug
        && old_context_window > new_context_window;
    if should_run {
        run_auto_compact(
            sess,
            &previous_model_turn_context,
            InitialContextInjection::DoNotInject,
            CompactionReason::ModelDownshift,
            CompactionPhase::PreTurn,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

async fn run_auto_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
    reason: CompactionReason,
    phase: CompactionPhase,
) -> CodexResult<()> {
    if should_use_remote_compact_task(turn_context.provider.info()) {
        run_inline_remote_auto_compact_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
            reason,
            phase,
        )
        .await?;
        begin_auto_compact_recovery(
            sess,
            turn_context,
            reason,
            phase,
            "remote_responses_compact",
        )
        .await?;
    } else {
        run_inline_auto_compact_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
            reason,
            phase,
        )
        .await?;
        begin_auto_compact_recovery(sess, turn_context, reason, phase, "local_inline").await?;
    }
    Ok(())
}

pub(crate) async fn begin_auto_compact_recovery(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    reason: CompactionReason,
    phase: CompactionPhase,
    implementation: &'static str,
) -> CodexResult<()> {
    let warning = resolved_post_compact_recovery_warning(turn_context);
    if let Err(error) = sess
        .begin_post_compact_recovery(turn_context, reason, phase, implementation, warning)
        .await
    {
        let event = EventMsg::Error(
            error.to_error_event(Some("Error preparing post-compact recovery".to_string())),
        );
        sess.send_event(turn_context, event).await;
        return Err(error);
    }
    Ok(())
}

fn resolved_post_compact_recovery_warning(turn_context: &TurnContext) -> String {
    let config = turn_context.config.as_ref();
    let is_subagent = matches!(&turn_context.session_source, SessionSource::SubAgent(_));
    let default_warning =
        crate::session::default_post_compact_recovery_warning(config, is_subagent).to_string();
    let Some(raw) = config.post_compact_recovery_warning.as_deref() else {
        return default_warning;
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return default_warning;
    }

    let without_warning_prefix = match trimmed.get(..8) {
        Some(prefix) if prefix.eq_ignore_ascii_case("warning:") => trimmed[8..].trim_start(),
        _ => trimmed,
    };
    let normalized = without_warning_prefix.trim();
    if normalized.is_empty() {
        return default_warning;
    }
    normalized.to_string()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UsageLimitHandlingPolicy {
    VisibleWarnAndAutoSwitch,
    HiddenSilentAutoSwitch,
}

#[derive(Default)]
pub(crate) struct AutoSwitchRefreshState {
    pub(crate) freshly_selectable_store_account_ids: HashSet<String>,
    pub(crate) freshly_unsupported_store_account_ids: HashSet<String>,
}

#[allow(clippy::too_many_arguments)]
#[instrument(
    level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug
    )
)]
pub(crate) async fn maybe_auto_switch_account_on_usage_limit(
    sess: &Session,
    turn_context: &TurnContext,
    usage_limit: &UsageLimitReachedError,
    request_store_account_id: Option<&str>,
    usage_limit_handling_policy: UsageLimitHandlingPolicy,
) -> CodexResult<bool> {
    let auth_mode = sess
        .services
        .auth_manager
        .auth_mode()
        .map_err(|error| CodexErr::Io(error.into_io_error()))?;
    if !matches!(
        auth_mode,
        Some(crate::auth::AuthMode::Chatgpt | crate::auth::AuthMode::ChatgptAuthTokens)
    ) {
        return Ok(false);
    }

    let refresh_state = refresh_accounts_rate_limits_before_auto_switch(sess, turn_context).await?;
    maybe_auto_switch_account_on_usage_limit_with_refreshed_account_state(
        sess,
        turn_context,
        usage_limit,
        request_store_account_id,
        &refresh_state,
        usage_limit_handling_policy,
    )
    .await
}

pub(crate) async fn maybe_auto_switch_account_on_usage_limit_with_refreshed_account_state(
    sess: &Session,
    turn_context: &TurnContext,
    usage_limit: &UsageLimitReachedError,
    request_store_account_id: Option<&str>,
    refresh_state: &AutoSwitchRefreshState,
    usage_limit_handling_policy: UsageLimitHandlingPolicy,
) -> CodexResult<bool> {
    let accounts_before = sess
        .services
        .auth_manager
        .account_manager()
        .list_accounts()
        .map_err(|error| CodexErr::Io(error.into_io_error()))?;
    let account_display_name = |account: &crate::auth::AccountSummary| {
        account
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .or_else(|| {
                account
                    .email
                    .as_deref()
                    .map(str::trim)
                    .filter(|email| !email.is_empty())
            })
            .unwrap_or(account.id.as_str())
            .to_string()
    };
    let active_account = accounts_before.iter().find(|account| account.is_active);
    let active_store_account_id = active_account.map(|account| account.id.clone());
    let failing_store_account_id = request_store_account_id
        .map(str::to_owned)
        .filter(|store_account_id| {
            accounts_before
                .iter()
                .any(|account| account.id == *store_account_id)
        })
        .or(active_store_account_id.clone());
    let protected_store_account_id =
        active_store_account_id
            .as_deref()
            .filter(|active_store_account_id| {
                Some(*active_store_account_id) != failing_store_account_id.as_deref()
            });
    let fallback_selection_mode = if protected_store_account_id.is_some() {
        UsageLimitAutoSwitchFallbackSelectionMode::CancelStaleRequestFallbackSelection
    } else {
        UsageLimitAutoSwitchFallbackSelectionMode::AllowFallbackSelection
    };

    let required_workspace_id = turn_context.config.forced_chatgpt_workspace_id.as_deref();
    let mutation = sess
        .services
        .auth_manager
        .account_manager()
        .switch_account_on_usage_limit(UsageLimitAutoSwitchRequest {
            required_workspace_id,
            failing_store_account_id: failing_store_account_id.as_deref(),
            resets_at: usage_limit.resets_at,
            snapshot: usage_limit.rate_limits.as_deref().cloned(),
            freshly_unsupported_store_account_ids: &refresh_state
                .freshly_unsupported_store_account_ids,
            protected_store_account_id,
            selection_scope: UsageLimitAutoSwitchSelectionScope::FreshlySelectable(
                &refresh_state.freshly_selectable_store_account_ids,
            ),
            fallback_selection_mode,
        })?;
    let switch_result = sess
        .services
        .auth_manager
        .refresh_auth_after_account_runtime_mutation(mutation);
    let Some(next_store_account_id) = switch_result else {
        let current_active_store_account_id = sess
            .services
            .auth_manager
            .account_manager()
            .list_accounts()
            .map_err(|error| CodexErr::Io(error.into_io_error()))?
            .into_iter()
            .find(|account| account.is_active)
            .map(|account| account.id);
        let should_retry_without_switch = stale_request_should_retry_without_switch(
            request_store_account_id,
            failing_store_account_id.as_deref(),
            current_active_store_account_id.as_deref(),
            refresh_state,
        );
        if should_retry_without_switch {
            debug!(
                active_store_account_id = ?active_store_account_id,
                current_active_store_account_id = ?current_active_store_account_id,
                failing_store_account_id = ?failing_store_account_id,
                "auto-switch skipped but active account changed since request; retrying without duplicate warning"
            );
            return Ok(true);
        }
        return Ok(false);
    };

    if active_store_account_id.as_deref() == Some(next_store_account_id.as_str()) {
        return Ok(false);
    }

    let accounts_after = sess
        .services
        .auth_manager
        .account_manager()
        .list_accounts()
        .map_err(|error| CodexErr::Io(error.into_io_error()))?;
    let from_account_name = failing_store_account_id
        .as_ref()
        .and_then(|account_id| {
            accounts_after
                .iter()
                .find(|account| account.id == *account_id)
                .or_else(|| {
                    accounts_before
                        .iter()
                        .find(|account| account.id == *account_id)
                })
        })
        .map(account_display_name)
        .unwrap_or_else(|| "<unknown>".to_string());
    let next_account_name = accounts_after
        .iter()
        .find(|account| account.id == next_store_account_id)
        .map(account_display_name)
        .unwrap_or_else(|| next_store_account_id.clone());
    if usage_limit_handling_policy == UsageLimitHandlingPolicy::VisibleWarnAndAutoSwitch {
        sess.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: format!(
                    "Usage limit reached for account '{from_account_name}'. Auto-switched to account '{next_account_name}' and retrying."
                ),
            }),
        )
        .await;
    }

    Ok(true)
}

pub(crate) async fn maybe_set_total_tokens_full_for_execution_mode(
    sess: &Session,
    turn_context: &TurnContext,
    execution_mode: SamplingExecutionMode,
) {
    // Merge-safety anchor: hidden prompt_gc retries must not emit visible token
    // accounting, or the background sidecar leaks into the lead turn UI.
    if execution_mode == SamplingExecutionMode::Visible {
        sess.set_total_tokens_full(turn_context).await;
    }
}

pub(crate) async fn handle_usage_limit_for_execution_mode(
    sess: &Session,
    turn_context: &TurnContext,
    usage_limit: &UsageLimitReachedError,
    request_store_account_id: Option<&str>,
    execution_mode: SamplingExecutionMode,
    usage_limit_handling_policy: UsageLimitHandlingPolicy,
) -> CodexResult<bool> {
    if let Some(rate_limits) = usage_limit.rate_limits.clone() {
        sess.update_rate_limits_with_visibility(
            turn_context,
            *rate_limits,
            execution_mode == SamplingExecutionMode::Visible,
        )
        .await;
    }
    if usage_limit_handling_policy == UsageLimitHandlingPolicy::VisibleWarnAndAutoSwitch
        || usage_limit_handling_policy == UsageLimitHandlingPolicy::HiddenSilentAutoSwitch
    {
        return maybe_auto_switch_account_on_usage_limit(
            sess,
            turn_context,
            usage_limit,
            request_store_account_id,
            usage_limit_handling_policy,
        )
        .await;
    }
    Ok(false)
}

pub(crate) fn stale_request_should_retry_without_switch(
    request_store_account_id: Option<&str>,
    failing_store_account_id: Option<&str>,
    current_active_store_account_id: Option<&str>,
    refresh_state: &AutoSwitchRefreshState,
) -> bool {
    request_store_account_id
        .zip(failing_store_account_id)
        .is_some_and(|(request_store_account_id, failing_store_account_id)| {
            request_store_account_id == failing_store_account_id
        })
        && failing_store_account_id != current_active_store_account_id
        && current_active_store_account_id.is_some_and(|store_account_id| {
            !refresh_state
                .freshly_unsupported_store_account_ids
                .contains(store_account_id)
        })
}

pub(crate) async fn maybe_emit_transport_fallback_warning_for_execution_mode(
    sess: &Session,
    turn_context: &TurnContext,
    execution_mode: SamplingExecutionMode,
    err: &CodexErr,
) {
    // Merge-safety anchor: transport fallback is part of the shared retry path,
    // but hidden prompt_gc runs must stay silent in the visible event stream.
    if execution_mode == SamplingExecutionMode::Visible {
        sess.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: format!("Falling back from WebSockets to HTTPS transport. {err:#}"),
            }),
        )
        .await;
    }
}

// Merge-safety anchor: usage-limit auto-switch must carry just-refreshed explicit unsupported
// proof from `/api/codex/usage` into the same auth-store mutation that selects the fallback
// account; ambiguous `Unknown` accounts must never enter this set.
pub(crate) fn auto_switch_refresh_account_ids_from_roster(
    roster: AccountRateLimitRefreshRoster,
) -> CodexResult<Vec<String>> {
    match roster.status {
        AccountRateLimitRefreshRosterStatus::LeaseManaged
        | AccountRateLimitRefreshRosterStatus::NoLeaseOwner => Ok(roster.store_account_ids),
        AccountRateLimitRefreshRosterStatus::LeaseReadFailed => {
            Err(CodexErr::Io(std::io::Error::other(
                "failed to read account lease state before usage-limit auto-switch",
            )))
        }
    }
}

pub(crate) async fn refresh_accounts_rate_limits_before_auto_switch(
    sess: &Session,
    turn_context: &TurnContext,
) -> CodexResult<AutoSwitchRefreshState> {
    if cfg!(test) && !turn_context.usage_limit_auto_switch_pre_refresh_enabled_in_tests() {
        return Ok(AutoSwitchRefreshState::default());
    }

    let refresh_roster = match sess
        .services
        .auth_manager
        .account_manager()
        .account_rate_limit_refresh_roster()
    {
        Ok(roster) => roster,
        Err(error) => {
            return Err(CodexErr::Io(error.into_io_error()));
        }
    };
    // Merge-safety anchor: usage-limit autoswitch may only restrict fallback
    // selection with known fresh roster/auth owner state; owner/lease/runtime
    // failures must surface instead of becoming empty or no-usable snapshot truth.
    let account_ids = auto_switch_refresh_account_ids_from_roster(refresh_roster)?;
    if account_ids.is_empty() {
        return Ok(AutoSwitchRefreshState::default());
    }
    let mut outcomes = Vec::new();
    let mut refresh_state = AutoSwitchRefreshState::default();
    let refreshed_at = Utc::now();
    for store_account_id in account_ids {
        let auth = match sess
            .services
            .auth_manager
            .resolve_chatgpt_auth_for_store_account_id(
                &store_account_id,
                ChatgptAccountRefreshMode::IfStale,
            )
            .await
        {
            Ok(ChatgptAccountAuthResolution::Auth(auth)) => auth,
            Ok(ChatgptAccountAuthResolution::Removed { error, .. }) => {
                debug!(
                    store_account_id = %store_account_id,
                    failed_reason = ?error.reason,
                    "removed saved ChatGPT account before usage-limit auto-switch refresh"
                );
                continue;
            }
            Ok(ChatgptAccountAuthResolution::Missing) => continue,
            Err(err) if err.is_account_runtime_load_error() => {
                return Err(CodexErr::Io(err.into()));
            }
            Err(err) => {
                debug!(
                    store_account_id = %store_account_id,
                    error = %err,
                    "failed to resolve ChatGPT account before usage-limit auto-switch"
                );
                outcomes.push((
                    store_account_id.clone(),
                    AccountRateLimitRefreshOutcome::NoUsableSnapshot,
                ));
                continue;
            }
        };
        let snapshot = match fetch_account_rate_limits_snapshot(
            &turn_context.config.chatgpt_base_url,
            auth.request_auth(),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(FetchAccountRateLimitsError::Unauthorized(error)) => {
                debug!(
                    store_account_id = %store_account_id,
                    error = %error,
                    "usage refresh hit unauthorized response before auto-switch; forcing per-account auth recovery"
                );
                let recovered_auth = match sess
                    .services
                    .auth_manager
                    .resolve_chatgpt_auth_for_store_account_id(
                        &store_account_id,
                        ChatgptAccountRefreshMode::Force,
                    )
                    .await
                {
                    Ok(ChatgptAccountAuthResolution::Auth(auth)) => auth,
                    Ok(ChatgptAccountAuthResolution::Removed { error, .. }) => {
                        debug!(
                            store_account_id = %store_account_id,
                            failed_reason = ?error.reason,
                            "removed saved ChatGPT account after unauthorized usage refresh before auto-switch"
                        );
                        continue;
                    }
                    Ok(ChatgptAccountAuthResolution::Missing) => continue,
                    Err(err) if err.is_account_runtime_load_error() => {
                        return Err(CodexErr::Io(err.into()));
                    }
                    Err(err) => {
                        debug!(
                            store_account_id = %store_account_id,
                            error = %err,
                            "failed to force-resolve ChatGPT account after unauthorized usage refresh"
                        );
                        outcomes.push((
                            store_account_id.clone(),
                            AccountRateLimitRefreshOutcome::NoUsableSnapshot,
                        ));
                        continue;
                    }
                };
                match fetch_account_rate_limits_snapshot(
                    &turn_context.config.chatgpt_base_url,
                    recovered_auth.request_auth(),
                )
                .await
                {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        debug!(
                            store_account_id = %store_account_id,
                            error = %err,
                            "failed to refresh account usage after unauthorized recovery before auto-switch"
                        );
                        outcomes.push((
                            store_account_id.clone(),
                            AccountRateLimitRefreshOutcome::NoUsableSnapshot,
                        ));
                        continue;
                    }
                }
            }
            Err(err) => {
                debug!(
                    store_account_id = %store_account_id,
                    error = %err,
                    "failed to refresh account usage before auto-switch"
                );
                outcomes.push((
                    store_account_id.clone(),
                    AccountRateLimitRefreshOutcome::NoUsableSnapshot,
                ));
                continue;
            }
        };
        if let Some(snapshot) = snapshot {
            if crate::auth::usage_limit_auto_switch_removes_plan_type(snapshot.plan_type.as_ref()) {
                refresh_state
                    .freshly_unsupported_store_account_ids
                    .insert(store_account_id.clone());
            }
            if auto_switch_refresh_snapshot_is_selectable(&snapshot, refreshed_at) {
                refresh_state
                    .freshly_selectable_store_account_ids
                    .insert(store_account_id.clone());
            }
            outcomes.push((
                store_account_id,
                AccountRateLimitRefreshOutcome::Snapshot(snapshot),
            ));
        } else {
            outcomes.push((
                store_account_id,
                AccountRateLimitRefreshOutcome::NoUsableSnapshot,
            ));
        }
    }

    if outcomes.is_empty() {
        return Ok(refresh_state);
    }

    match sess
        .services
        .auth_manager
        .account_manager()
        .reconcile_account_rate_limit_refresh_outcomes(outcomes)
    {
        Ok(updated_accounts) => {
            debug!(
                updated_accounts,
                "refreshed account usage cache before auto-switch"
            );
        }
        Err(err) => {
            warn!(
                error = %err,
                "failed to persist refreshed account usage before auto-switch"
            );
        }
    }

    Ok(refresh_state)
}

fn auto_switch_refresh_snapshot_is_selectable(
    snapshot: &RateLimitSnapshot,
    now: DateTime<Utc>,
) -> bool {
    !auto_switch_refresh_window_is_blocked(snapshot.primary.as_ref(), now)
        && !auto_switch_refresh_window_is_blocked(snapshot.secondary.as_ref(), now)
}

fn auto_switch_refresh_window_is_blocked(
    window: Option<&RateLimitWindow>,
    now: DateTime<Utc>,
) -> bool {
    let Some(window) = window else {
        return false;
    };

    window.is_depleted_at(now.timestamp())
}

#[derive(Debug)]
enum FetchAccountRateLimitsError {
    Unauthorized(String),
    Other(String),
}

impl std::fmt::Display for FetchAccountRateLimitsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized(message) | Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for FetchAccountRateLimitsError {}

async fn fetch_account_rate_limits_snapshot(
    chatgpt_base_url: &str,
    auth: &ChatGptRequestAuth,
) -> Result<Option<RateLimitSnapshot>, FetchAccountRateLimitsError> {
    let client =
        codex_backend_client::Client::from_chatgpt_request_auth(chatgpt_base_url.to_string(), auth)
            .map_err(|err| {
                FetchAccountRateLimitsError::Other(format!("usage client init failed: {err}"))
            })?;
    match client.get_rate_limits_detailed().await {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(err) => Err(if err.is_unauthorized() {
            FetchAccountRateLimitsError::Unauthorized(format!("usage request failed: {err}"))
        } else {
            FetchAccountRateLimitsError::Other(format!("usage request failed: {err}"))
        }),
    }
}

pub(super) fn collect_explicit_app_ids_from_skill_items(
    skill_items: &[ResponseItem],
    connectors: &[connectors::AppInfo],
    skill_name_counts_lower: &HashMap<String, usize>,
) -> HashSet<String> {
    if skill_items.is_empty() || connectors.is_empty() {
        return HashSet::new();
    }

    let skill_messages = skill_items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { content, .. } => {
                content.iter().find_map(|content_item| match content_item {
                    ContentItem::InputText { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<String>>();
    if skill_messages.is_empty() {
        return HashSet::new();
    }

    let mentions = collect_tool_mentions_from_messages(&skill_messages);
    let mention_names_lower = mentions
        .plain_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<String>>();
    let mut connector_ids = mentions
        .paths
        .iter()
        .filter(|path| tool_kind_for_path(path) == ToolMentionKind::App)
        .filter_map(|path| app_id_from_path(path).map(str::to_string))
        .collect::<HashSet<String>>();

    let connector_slug_counts = build_connector_slug_counts(connectors);
    for connector in connectors {
        let slug = codex_connectors::metadata::connector_mention_slug(connector);
        let connector_count = connector_slug_counts.get(&slug).copied().unwrap_or(0);
        let skill_count = skill_name_counts_lower.get(&slug).copied().unwrap_or(0);
        if connector_count == 1 && skill_count == 0 && mention_names_lower.contains(&slug) {
            connector_ids.insert(connector.id.clone());
        }
    }

    connector_ids
}

pub(super) fn filter_connectors_for_input(
    connectors: &[connectors::AppInfo],
    input: &[ResponseItem],
    explicitly_enabled_connectors: &HashSet<String>,
    skill_name_counts_lower: &HashMap<String, usize>,
) -> Vec<connectors::AppInfo> {
    let connectors: Vec<connectors::AppInfo> = connectors
        .iter()
        .filter(|connector| connector.is_enabled)
        .cloned()
        .collect::<Vec<_>>();
    if connectors.is_empty() {
        return Vec::new();
    }

    let user_messages = collect_user_messages(input);
    if user_messages.is_empty() && explicitly_enabled_connectors.is_empty() {
        return Vec::new();
    }

    let mentions = collect_tool_mentions_from_messages(&user_messages);
    let mention_names_lower = mentions
        .plain_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<String>>();

    let connector_slug_counts = build_connector_slug_counts(&connectors);
    let mut allowed_connector_ids = explicitly_enabled_connectors.clone();
    for path in mentions
        .paths
        .iter()
        .filter(|path| tool_kind_for_path(path) == ToolMentionKind::App)
    {
        if let Some(connector_id) = app_id_from_path(path) {
            allowed_connector_ids.insert(connector_id.to_string());
        }
    }

    connectors
        .into_iter()
        .filter(|connector| {
            connector_inserted_in_messages(
                connector,
                &mention_names_lower,
                &allowed_connector_ids,
                &connector_slug_counts,
                skill_name_counts_lower,
            )
        })
        .collect()
}

fn connector_inserted_in_messages(
    connector: &connectors::AppInfo,
    mention_names_lower: &HashSet<String>,
    allowed_connector_ids: &HashSet<String>,
    connector_slug_counts: &HashMap<String, usize>,
    skill_name_counts_lower: &HashMap<String, usize>,
) -> bool {
    if allowed_connector_ids.contains(&connector.id) {
        return true;
    }

    let mention_slug = codex_connectors::metadata::connector_mention_slug(connector);
    let connector_count = connector_slug_counts
        .get(&mention_slug)
        .copied()
        .unwrap_or(0);
    let skill_count = skill_name_counts_lower
        .get(&mention_slug)
        .copied()
        .unwrap_or(0);
    connector_count == 1 && skill_count == 0 && mention_names_lower.contains(&mention_slug)
}

pub(crate) fn build_prompt(
    input: Vec<ResponseItem>,
    router: &ToolRouter,
    turn_context: &TurnContext,
    base_instructions: BaseInstructions,
) -> Prompt {
    let deferred_dynamic_tools = turn_context
        .dynamic_tools
        .iter()
        .filter(|tool| tool.defer_loading)
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let tools = if deferred_dynamic_tools.is_empty() {
        router.model_visible_specs()
    } else {
        router
            .model_visible_specs()
            .into_iter()
            .filter(|spec| !deferred_dynamic_tools.contains(spec.name()))
            .collect()
    };

    Prompt {
        input,
        tools,
        parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
        base_instructions,
        personality: turn_context.personality,
        output_schema: turn_context.final_output_json_schema.clone(),
    }
}

fn structured_function_call_error_payload(
    error: &crate::function_tool::FunctionCallError,
) -> Option<Value> {
    let crate::function_tool::FunctionCallError::RespondToModel(raw) = error else {
        return None;
    };
    serde_json::from_str(raw).ok()
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct PromptGcPlanBuildFailureDetails {
    pub(super) error_message: String,
    pub(super) marker_stop_reason: String,
    pub(super) status_error: String,
    pub(super) blocks_remaining_turn: bool,
}

pub(super) fn prompt_gc_plan_build_failure_details(
    error: &crate::function_tool::FunctionCallError,
) -> PromptGcPlanBuildFailureDetails {
    let error_message = error.to_string();
    let error_payload = structured_function_call_error_payload(error);
    let stop_reason = error_payload
        .as_ref()
        .and_then(|payload| payload.get("stop_reason"))
        .and_then(Value::as_str);
    let status_error = error_payload
        .as_ref()
        .and_then(|payload| payload.get("message"))
        .and_then(Value::as_str)
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| error_message.clone());
    PromptGcPlanBuildFailureDetails {
        error_message,
        marker_stop_reason: stop_reason.unwrap_or("plan_build_failed").to_string(),
        status_error,
        blocks_remaining_turn: matches!(
            stop_reason,
            Some(
                "state_hash_mismatch" | "incompatible_rollout_history" | "missing_rollout_recorder"
            )
        ),
    }
}

async fn persist_prompt_gc_rollout_marker(
    sess: &Session,
    checkpoint: &crate::prompt_gc_sidecar::PromptGcCheckpoint,
    kind: PromptGcOutcomeKind,
    phase: Option<PromptGcExecutionPhase>,
    stop_reason: Option<String>,
    error_message: Option<String>,
    applied_unit_count: Option<u64>,
) {
    sess.persist_rollout_items(&[RolloutItem::Compacted(CompactedItem {
        message: PROMPT_GC_COMPACTION_MESSAGE.to_string(),
        replacement_history: None,
        prompt_gc: Some(PromptGcCompactionMetadata {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            checkpoint_seq: checkpoint.checkpoint_seq,
            kind,
            phase,
            stop_reason,
            error_message,
            applied_unit_count,
        }),
    })])
    .await;
}

pub(super) async fn run_prompt_gc_sidecar_if_needed(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    turn_diff_tracker: SharedTurnDiffTracker,
    parent_client_session: &ModelClientSession,
    cancellation_token: CancellationToken,
) {
    let Some(sidecar) = sess
        .prompt_gc_sidecar_for_sub_id(&turn_context.sub_id)
        .await
    else {
        return;
    };
    let checkpoint = {
        let mut sidecar = sidecar.lock().await;
        sidecar.recover_noted_apply_outcome();
        sidecar.take_pending_checkpoint()
    };
    let Some(checkpoint) = checkpoint else {
        return;
    };
    let checkpoint_eligibility = sidecar
        .lock()
        .await
        .checkpoint_eligibility(&checkpoint.checkpoint_id);
    // Merge-safety anchor: prompt_gc keeps the strict `FunctionCallOutput`
    // fast path for non-final checkpoints, but final answers may fall back to
    // selectable raw burden derived from the retrieve budget. Keep this
    // predicate aligned with `PromptGcCheckpointEligibility` and the AGENTS
    // inventory entry.
    let final_answer_fallback_eligible = matches!(
        checkpoint_eligibility,
        Some(eligibility)
            if checkpoint.phase == MessagePhase::FinalAnswer
                && eligibility.triggering_function_call_output_count == 0
                && eligibility.selectable_raw_bytes
                    >= crate::prompt_gc_sidecar::PROMPT_GC_MIN_FINAL_ANSWER_SELECTABLE_RAW_BYTES
    );
    if matches!(
        checkpoint_eligibility,
        Some(eligibility)
            if eligibility.selectable_unit_count > 0
                && eligibility.triggering_function_call_output_count == 0
                && !final_answer_fallback_eligible
    ) {
        debug!(
            turn_id = %turn_context.sub_id,
            checkpoint_id = %checkpoint.checkpoint_id,
            phase = ?checkpoint.phase,
            checkpoint_eligibility = ?checkpoint_eligibility,
            threshold_tokens = crate::prompt_gc_sidecar::PROMPT_GC_MIN_FUNCTION_CALL_OUTPUT_TOKEN_QTY,
            threshold_selectable_raw_bytes = crate::prompt_gc_sidecar::PROMPT_GC_MIN_FINAL_ANSWER_SELECTABLE_RAW_BYTES,
            "skipping prompt_gc checkpoint until a function_call_output reports Token qty above threshold or a final answer reaches the selectable burden fallback"
        );
        sidecar.lock().await.skip_cycle(&checkpoint.checkpoint_id);
        return;
    }
    persist_prompt_gc_rollout_marker(
        sess.as_ref(),
        &checkpoint,
        PromptGcOutcomeKind::Started,
        Some(PromptGcExecutionPhase::Prepare),
        /*stop_reason*/ None,
        /*error_message*/ None,
        /*applied_unit_count*/ None,
    )
    .await;
    struct PromptGcActivityGuard<'a> {
        session: &'a Session,
        refresh_private_context_usage: bool,
    }
    impl<'a> PromptGcActivityGuard<'a> {
        fn new(session: &'a Session) -> Self {
            session.set_prompt_gc_activity(/*active*/ true);
            Self {
                session,
                refresh_private_context_usage: false,
            }
        }

        fn mark_context_usage_refresh(&mut self) {
            self.refresh_private_context_usage = true;
        }
    }
    impl Drop for PromptGcActivityGuard<'_> {
        fn drop(&mut self) {
            self.session
                .clear_prompt_gc_activity(self.refresh_private_context_usage);
        }
    }
    let mut prompt_gc_activity_guard = PromptGcActivityGuard::new(sess.as_ref());

    let plan = match crate::tools::handlers::prompt_gc::build_runtime_plan(
        sess.as_ref(),
        turn_context.as_ref(),
        &checkpoint.checkpoint_id,
    )
    .await
    {
        Ok(plan) => plan,
        Err(error) => {
            let failure = prompt_gc_plan_build_failure_details(&error);
            warn!(
                turn_id = %turn_context.sub_id,
                checkpoint_id = %checkpoint.checkpoint_id,
                error = %error,
                "prompt_gc sidecar failed to build the runtime plan"
            );
            let mut sidecar = sidecar.lock().await;
            if failure.blocks_remaining_turn {
                sidecar.block_remaining_turn(&checkpoint.checkpoint_id, failure.status_error);
            } else {
                sidecar.fail_cycle(&checkpoint.checkpoint_id, failure.error_message.clone());
            }
            persist_prompt_gc_rollout_marker(
                sess.as_ref(),
                &checkpoint,
                PromptGcOutcomeKind::Failed,
                Some(PromptGcExecutionPhase::Prepare),
                Some(failure.marker_stop_reason),
                Some(failure.error_message),
                /*applied_unit_count*/ None,
            )
            .await;
            return;
        }
    };

    if plan.chunk_manifest.is_empty() {
        let outcome = crate::prompt_gc_sidecar::PromptGcApplyOutcome {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            checkpoint_seq: checkpoint.checkpoint_seq,
            applied_unit_keys: Vec::new(),
        };
        sidecar.lock().await.complete_cycle(outcome);
        persist_prompt_gc_rollout_marker(
            sess.as_ref(),
            &checkpoint,
            PromptGcOutcomeKind::NoEligibleChunks,
            Some(PromptGcExecutionPhase::Prepare),
            Some("no_eligible_chunks".to_string()),
            /*error_message*/ None,
            Some(0),
        )
        .await;
        return;
    }

    let chunk_manifest = plan
        .chunk_manifest
        .iter()
        .map(|chunk| chunk.manifest.clone())
        .collect::<Vec<_>>();
    let input = match prompt_gc_summary_input(&checkpoint, &chunk_manifest) {
        Ok(input) => input,
        Err(error) => {
            warn!(
                turn_id = %turn_context.sub_id,
                checkpoint_id = %checkpoint.checkpoint_id,
                error = %error,
                "prompt_gc sidecar failed to build summary input"
            );
            sidecar
                .lock()
                .await
                .fail_cycle(&checkpoint.checkpoint_id, error.clone());
            persist_prompt_gc_rollout_marker(
                sess.as_ref(),
                &checkpoint,
                PromptGcOutcomeKind::Failed,
                Some(PromptGcExecutionPhase::Prepare),
                Some("summary_input_build_failed".to_string()),
                Some(error),
                /*applied_unit_count*/ None,
            )
            .await;
            return;
        }
    };

    let router = Arc::new(crate::tools::router::ToolRouter::from_builder(
        crate::tools::registry::ToolRegistryBuilder::new(),
    ));
    let prompt = Prompt {
        input,
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: crate::client_common::PROMPT_GC_PROMPT.to_string(),
        },
        personality: None,
        output_schema: Some(prompt_gc_summary_output_schema()),
    };
    let mut client_session = parent_client_session.new_hidden_child_session();
    let mut server_model_warning_emitted = false;
    let result = run_sampling_request_with_router_and_prompt(
        Arc::clone(sess),
        Arc::clone(turn_context),
        turn_diff_tracker,
        &mut client_session,
        /*turn_metadata_header*/ None,
        router,
        prompt,
        &mut server_model_warning_emitted,
        cancellation_token.child_token(),
        SamplingExecutionMode::Hidden,
        UsageLimitHandlingPolicy::HiddenSilentAutoSwitch,
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(CodexErr::UsageLimitReached(error)) => {
            warn!(
                turn_id = %turn_context.sub_id,
                checkpoint_id = %checkpoint.checkpoint_id,
                error = %error,
                "prompt_gc sidecar request hit an unrecoverable usage limit"
            );
            sidecar
                .lock()
                .await
                .block_remaining_turn(&checkpoint.checkpoint_id, error.to_string());
            persist_prompt_gc_rollout_marker(
                sess.as_ref(),
                &checkpoint,
                PromptGcOutcomeKind::Failed,
                Some(PromptGcExecutionPhase::Request),
                Some("usage_limit_reached".to_string()),
                Some(error.to_string()),
                /*applied_unit_count*/ None,
            )
            .await;
            return;
        }
        Err(error) => {
            warn!(
                turn_id = %turn_context.sub_id,
                checkpoint_id = %checkpoint.checkpoint_id,
                error = %error,
                "prompt_gc sidecar request failed"
            );
            sidecar
                .lock()
                .await
                .fail_cycle(&checkpoint.checkpoint_id, error.to_string());
            persist_prompt_gc_rollout_marker(
                sess.as_ref(),
                &checkpoint,
                PromptGcOutcomeKind::Failed,
                Some(PromptGcExecutionPhase::Request),
                Some("request_failed".to_string()),
                Some(error.to_string()),
                /*applied_unit_count*/ None,
            )
            .await;
            return;
        }
    };

    if result.needs_follow_up {
        sidecar.lock().await.fail_cycle(
            &checkpoint.checkpoint_id,
            "prompt_gc sidecar requested an unexpected follow-up",
        );
        persist_prompt_gc_rollout_marker(
            sess.as_ref(),
            &checkpoint,
            PromptGcOutcomeKind::Failed,
            Some(PromptGcExecutionPhase::Request),
            Some("unexpected_follow_up".to_string()),
            Some("prompt_gc sidecar requested an unexpected follow-up".to_string()),
            /*applied_unit_count*/ None,
        )
        .await;
        return;
    }

    let summaries = match parse_prompt_gc_summary_response(&result.non_tool_response_items) {
        Ok(summaries) => summaries,
        Err(error) => {
            warn!(
                turn_id = %turn_context.sub_id,
                checkpoint_id = %checkpoint.checkpoint_id,
                error = %error,
                "prompt_gc sidecar returned an invalid summary payload"
            );
            sidecar
                .lock()
                .await
                .fail_cycle(&checkpoint.checkpoint_id, error.clone());
            persist_prompt_gc_rollout_marker(
                sess.as_ref(),
                &checkpoint,
                PromptGcOutcomeKind::Failed,
                Some(PromptGcExecutionPhase::Summarize),
                Some("invalid_summary_payload".to_string()),
                Some(error),
                /*applied_unit_count*/ None,
            )
            .await;
            return;
        }
    };

    match crate::tools::handlers::prompt_gc::apply_runtime_plan(
        sess.as_ref(),
        turn_context.as_ref(),
        &checkpoint,
        &plan,
        &summaries,
    )
    .await
    {
        Ok(outcome) => {
            prompt_gc_activity_guard.mark_context_usage_refresh();
            sidecar.lock().await.complete_cycle(outcome);
        }
        Err(error) => {
            warn!(
                turn_id = %turn_context.sub_id,
                checkpoint_id = %checkpoint.checkpoint_id,
                error = %error,
                "prompt_gc sidecar failed to apply the runtime summary"
            );
            sidecar
                .lock()
                .await
                .fail_cycle(&checkpoint.checkpoint_id, error.to_string());
            persist_prompt_gc_rollout_marker(
                sess.as_ref(),
                &checkpoint,
                PromptGcOutcomeKind::Failed,
                Some(PromptGcExecutionPhase::Apply),
                Some("apply_failed".to_string()),
                Some(error.to_string()),
                /*applied_unit_count*/ None,
            )
            .await;
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PromptGcSummaryResponse {
    summaries: Vec<crate::tools::handlers::prompt_gc::PromptGcChunkSummary>,
}

fn prompt_gc_summary_output_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summaries": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "chunk_id": { "type": "string" },
                        "tool_context": { "type": "string" },
                        "reasoning_context": { "type": "string" }
                    },
                    "required": ["chunk_id", "tool_context", "reasoning_context"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["summaries"],
        "additionalProperties": false
    })
}

pub(super) fn parse_prompt_gc_summary_response(
    items: &[ResponseItem],
) -> Result<Vec<crate::tools::handlers::prompt_gc::PromptGcChunkSummary>, String> {
    let assistant_messages = items
        .iter()
        .filter(|item| matches!(item, ResponseItem::Message { role, .. } if role == "assistant"))
        .collect::<Vec<_>>();
    if assistant_messages.is_empty() {
        return Err("prompt_gc sidecar returned no assistant summary payload".to_string());
    }
    if assistant_messages.len() != 1 {
        return Err(format!(
            "prompt_gc sidecar requires exactly one assistant summary payload, got {}",
            assistant_messages.len()
        ));
    }
    let assistant_payload = raw_assistant_output_text_from_item(assistant_messages[0])
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "prompt_gc sidecar returned no assistant summary payload".to_string())?;
    parse_prompt_gc_summary_response_text(&assistant_payload).map(|response| response.summaries)
}

pub(super) fn parse_prompt_gc_summary_response_text(
    raw: &str,
) -> Result<PromptGcSummaryResponse, String> {
    let response: PromptGcSummaryResponse = serde_json::from_str(raw)
        .map_err(|error| format!("failed to parse prompt_gc summary JSON: {error}"))?;
    if response.summaries.is_empty() {
        return Err("prompt_gc summary response requires a non-empty summaries list".to_string());
    }
    Ok(response)
}

fn prompt_gc_summary_input_message(
    checkpoint: &crate::prompt_gc_sidecar::PromptGcCheckpoint,
    chunk_manifest: &[crate::tools::handlers::prompt_gc::PromptGcChunkManifestEntry],
) -> Result<String, String> {
    let manifest_text = serde_json::to_string_pretty(chunk_manifest)
        .map_err(|error| format!("failed to serialize prompt_gc chunk_manifest: {error}"))?;
    Ok(format!(
        "mode=prompt_gc_summary\ncheckpoint_id={}\ncheckpoint_seq={}\n\nReturn JSON only. Do not emit prose, markdown, or code fences.\n\nRequirements:\n- Return exactly one summary object per chunk_manifest entry.\n- Use only chunk_id values from chunk_manifest.\n- Keep chunk_id values unique.\n- Preserve semantic meaning while reducing prompt bloat.\n- `tool_context` and `reasoning_context` may be empty individually, but not both for the same chunk.\n\nchunk_manifest:\n{manifest_text}",
        checkpoint.checkpoint_id, checkpoint.checkpoint_seq,
    ))
}

fn prompt_gc_summary_input(
    checkpoint: &crate::prompt_gc_sidecar::PromptGcCheckpoint,
    chunk_manifest: &[crate::tools::handlers::prompt_gc::PromptGcChunkManifestEntry],
) -> Result<Vec<ResponseItem>, String> {
    let text = prompt_gc_summary_input_message(checkpoint, chunk_manifest)?;
    Ok(vec![ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text }],
        end_turn: None,
        phase: None,
    }])
}

#[allow(clippy::too_many_arguments)]
async fn run_sampling_request_with_router_and_prompt(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_diff_tracker: SharedTurnDiffTracker,
    client_session: &mut ModelClientSession,
    turn_metadata_header: Option<&str>,
    router: Arc<ToolRouter>,
    prompt: Prompt,
    server_model_warning_emitted_for_turn: &mut bool,
    cancellation_token: CancellationToken,
    execution_mode: SamplingExecutionMode,
    usage_limit_handling_policy: UsageLimitHandlingPolicy,
) -> CodexResult<SamplingRequestResult> {
    let tool_runtime = ToolCallRuntime::new(
        Arc::clone(&router),
        Arc::clone(&sess),
        Arc::clone(&turn_context),
        Arc::clone(&turn_diff_tracker),
        /*allowed_tool_names*/ None,
        ToolCallSource::Direct,
    );
    let _code_mode_worker = sess
        .services
        .code_mode_service
        .start_turn_worker(
            &sess,
            &turn_context,
            Arc::clone(&router),
            Arc::clone(&turn_diff_tracker),
        )
        .await;
    let mut retries = 0;
    loop {
        let request_store_account_id = sess
            .services
            .auth_manager
            .account_manager()
            .list_accounts()
            .map_err(|error| CodexErr::Io(error.into_io_error()))?
            .into_iter()
            .find(|account| account.is_active)
            .map(|account| account.id);
        let err = match try_run_sampling_request(
            tool_runtime.clone(),
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            client_session,
            turn_metadata_header,
            Arc::clone(&turn_diff_tracker),
            server_model_warning_emitted_for_turn,
            &prompt,
            cancellation_token.child_token(),
            execution_mode,
        )
        .await
        {
            Ok(output) => {
                return Ok(output);
            }
            Err(CodexErr::ContextWindowExceeded) => {
                maybe_set_total_tokens_full_for_execution_mode(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    execution_mode,
                )
                .await;
                return Err(CodexErr::ContextWindowExceeded);
            }
            Err(CodexErr::UsageLimitReached(error)) => {
                if handle_usage_limit_for_execution_mode(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &error,
                    request_store_account_id.as_deref(),
                    execution_mode,
                    usage_limit_handling_policy,
                )
                .await?
                {
                    continue;
                }
                return Err(CodexErr::UsageLimitReached(error));
            }
            Err(err) => err,
        };

        if !err.is_retryable() {
            return Err(err);
        }

        let max_retries = turn_context.provider.info().stream_max_retries();
        if retries >= max_retries
            && client_session.try_switch_fallback_transport(
                &turn_context.session_telemetry,
                &turn_context.model_info,
            )
        {
            maybe_emit_transport_fallback_warning_for_execution_mode(
                sess.as_ref(),
                turn_context.as_ref(),
                execution_mode,
                &err,
            )
            .await;
            retries = 0;
            continue;
        }
        if retries < max_retries {
            retries += 1;
            let delay = match &err {
                CodexErr::Stream(_, requested_delay) => {
                    requested_delay.unwrap_or_else(|| backoff(retries))
                }
                _ => backoff(retries),
            };
            warn!(
                "stream disconnected - retrying sampling request ({retries}/{max_retries} in {delay:?})...",
            );
            tokio::time::sleep(delay).await;
        } else {
            return Err(err);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug,
        cwd = %turn_context.cwd.display()
    )
)]
pub(crate) async fn run_sampling_request(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_diff_tracker: SharedTurnDiffTracker,
    client_session: &mut ModelClientSession,
    turn_metadata_header: Option<&str>,
    input: Vec<ResponseItem>,
    explicitly_enabled_connectors: &HashSet<String>,
    skills_outcome: Option<&SkillLoadOutcome>,
    allowed_tool_names: Option<&HashSet<String>>,
    server_model_warning_emitted_for_turn: &mut bool,
    cancellation_token: CancellationToken,
) -> CodexResult<SamplingRequestResult> {
    let router = built_tools(
        sess.as_ref(),
        turn_context.as_ref(),
        &input,
        explicitly_enabled_connectors,
        skills_outcome,
        &cancellation_token,
    )
    .await?;

    let base_instructions = sess.get_base_instructions().await;

    let tool_runtime = ToolCallRuntime::new(
        Arc::clone(&router),
        Arc::clone(&sess),
        Arc::clone(&turn_context),
        Arc::clone(&turn_diff_tracker),
        allowed_tool_names.cloned(),
        ToolCallSource::Direct,
    );
    let _code_mode_worker = sess
        .services
        .code_mode_service
        .start_turn_worker(
            &sess,
            &turn_context,
            Arc::clone(&router),
            Arc::clone(&turn_diff_tracker),
        )
        .await;
    let mut retries = 0;
    let mut initial_input = Some(input);
    loop {
        let prompt_input = if let Some(input) = initial_input.take() {
            input
        } else {
            sess.clone_history()
                .await
                .for_prompt(&turn_context.model_info.input_modalities)
        };
        let prompt_input = sess
            .inject_pending_post_compact_recovery(prompt_input)
            .await;
        let mut prompt = build_prompt(
            prompt_input,
            router.as_ref(),
            turn_context.as_ref(),
            base_instructions.clone(),
        );
        if let Some(allowed_tool_names) = allowed_tool_names {
            prompt
                .tools
                .retain(|spec| allowed_tool_names.contains(spec.name()));
            if !allowed_tool_names.is_empty() && prompt.tools.is_empty() {
                return Err(CodexErr::InvalidRequest(
                    "no allowed tools are available for this request".to_string(),
                ));
            }
        }
        let request_store_account_id = sess
            .services
            .auth_manager
            .account_manager()
            .list_accounts()
            .map_err(|error| CodexErr::Io(error.into_io_error()))?
            .into_iter()
            .find(|account| account.is_active)
            .map(|account| account.id);

        let err = match try_run_sampling_request(
            tool_runtime.clone(),
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            client_session,
            turn_metadata_header,
            Arc::clone(&turn_diff_tracker),
            server_model_warning_emitted_for_turn,
            &prompt,
            cancellation_token.child_token(),
            SamplingExecutionMode::Visible,
        )
        .await
        {
            Ok(output) => {
                return Ok(output);
            }
            Err(CodexErr::ContextWindowExceeded) => {
                sess.set_total_tokens_full(&turn_context).await;
                return Err(CodexErr::ContextWindowExceeded);
            }
            Err(CodexErr::UsageLimitReached(error)) => {
                if handle_usage_limit_for_execution_mode(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &error,
                    request_store_account_id.as_deref(),
                    SamplingExecutionMode::Visible,
                    UsageLimitHandlingPolicy::VisibleWarnAndAutoSwitch,
                )
                .await?
                {
                    continue;
                }
                return Err(CodexErr::UsageLimitReached(error));
            }
            Err(err) => err,
        };

        if !err.is_retryable() {
            return Err(err);
        }

        // Use the configured provider-specific stream retry budget.
        let max_retries = turn_context.provider.info().stream_max_retries();
        if retries >= max_retries
            && client_session.try_switch_fallback_transport(
                &turn_context.session_telemetry,
                &turn_context.model_info,
            )
        {
            sess.send_event(
                &turn_context,
                EventMsg::Warning(WarningEvent {
                    message: format!("Falling back from WebSockets to HTTPS transport. {err:#}"),
                }),
            )
            .await;
            retries = 0;
            continue;
        }
        if retries < max_retries {
            retries += 1;
            let delay = match &err {
                CodexErr::Stream(_, requested_delay) => {
                    requested_delay.unwrap_or_else(|| backoff(retries))
                }
                _ => backoff(retries),
            };
            warn!(
                "stream disconnected - retrying sampling request ({retries}/{max_retries} in {delay:?})...",
            );

            // In release builds, hide the first websocket retry notification to reduce noisy
            // transient reconnect messages. In debug builds, keep full visibility for diagnosis.
            let report_error = retries > 1
                || cfg!(debug_assertions)
                || !sess.services.model_client.responses_websocket_enabled();
            if report_error {
                // Surface retry information to any UI/front‑end so the
                // user understands what is happening instead of staring
                // at a seemingly frozen screen.
                sess.notify_stream_error(
                    &turn_context,
                    format!("Reconnecting... {retries}/{max_retries}"),
                    err,
                )
                .await;
            }
            tokio::time::sleep(delay).await;
        } else {
            return Err(err);
        }
    }
}

pub(crate) async fn built_tools(
    sess: &Session,
    turn_context: &TurnContext,
    input: &[ResponseItem],
    explicitly_enabled_connectors: &HashSet<String>,
    skills_outcome: Option<&SkillLoadOutcome>,
    cancellation_token: &CancellationToken,
) -> CodexResult<Arc<ToolRouter>> {
    let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
    let has_mcp_servers = mcp_connection_manager.has_servers();
    let all_mcp_tools = mcp_connection_manager
        .list_all_tools()
        .or_cancel(cancellation_token)
        .await?;
    drop(mcp_connection_manager);
    let loaded_plugins = sess
        .services
        .plugins_manager
        .plugins_for_config(&turn_context.config)
        .await;

    let mut effective_explicitly_enabled_connectors = explicitly_enabled_connectors.clone();
    effective_explicitly_enabled_connectors.extend(sess.get_connector_selection().await);

    let apps_enabled = turn_context.apps_enabled()?;
    let accessible_connectors =
        apps_enabled.then(|| connectors::accessible_connectors_from_mcp_tools(&all_mcp_tools));
    let accessible_connectors_with_enabled_state =
        accessible_connectors.as_ref().map(|connectors| {
            connectors::with_app_enabled_state(connectors.clone(), &turn_context.config)
        });
    let connectors = if apps_enabled {
        let connectors = codex_connectors::merge::merge_plugin_connectors_with_accessible(
            loaded_plugins
                .effective_apps()
                .into_iter()
                .map(|connector_id| connector_id.0),
            accessible_connectors.clone().unwrap_or_default(),
        );
        Some(connectors::with_app_enabled_state(
            connectors,
            &turn_context.config,
        ))
    } else {
        None
    };
    let auth = sess
        .services
        .auth_manager
        .chatgpt_request_auth()
        .await
        .map_err(|error| CodexErr::Io(error.into_io_error()))?;
    let discoverable_tools = if apps_enabled && turn_context.tools_config.tool_suggest {
        if let Some(accessible_connectors) = accessible_connectors_with_enabled_state.as_ref() {
            match connectors::list_tool_suggest_discoverable_tools_with_auth(
                &turn_context.config,
                auth.as_ref(),
                accessible_connectors.as_slice(),
            )
            .await
            .map(|discoverable_tools| {
                filter_tool_suggest_discoverable_tools_for_client(
                    discoverable_tools,
                    turn_context.app_server_client_name.as_deref(),
                )
            }) {
                Ok(discoverable_tools) if discoverable_tools.is_empty() => None,
                Ok(discoverable_tools) => Some(discoverable_tools),
                Err(err) => {
                    warn!("failed to load discoverable tool suggestions: {err:#}");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let explicitly_enabled = if let Some(connectors) = connectors.as_ref() {
        let skill_name_counts_lower = skills_outcome.map_or_else(HashMap::new, |outcome| {
            build_skill_name_counts(&outcome.skills, &outcome.disabled_paths).1
        });

        filter_connectors_for_input(
            connectors,
            input,
            &effective_explicitly_enabled_connectors,
            &skill_name_counts_lower,
        )
    } else {
        Vec::new()
    };
    let mcp_tool_exposure = build_mcp_tool_exposure(
        &all_mcp_tools,
        connectors.as_deref(),
        explicitly_enabled.as_slice(),
        &turn_context.config,
        &turn_context.tools_config,
    );
    let mcp_tools = has_mcp_servers.then_some(mcp_tool_exposure.direct_tools);
    let deferred_mcp_tools = mcp_tool_exposure.deferred_tools;
    let unavailable_called_tools = if turn_context
        .config
        .features
        .enabled(Feature::UnavailableDummyTools)
    {
        let exposed_tool_names = mcp_tools
            .iter()
            .chain(deferred_mcp_tools.iter())
            .flat_map(|tools| tools.keys().map(String::as_str))
            .collect::<HashSet<_>>();
        collect_unavailable_called_tools(input, &exposed_tool_names)
    } else {
        Vec::new()
    };

    let parallel_mcp_server_names = turn_context
        .config
        .mcp_servers
        .get()
        .iter()
        .filter_map(|(server_name, server_config)| {
            server_config
                .supports_parallel_tool_calls
                .then_some(server_name.clone())
        })
        .collect::<HashSet<_>>();

    Ok(Arc::new(ToolRouter::from_config(
        &turn_context.tools_config,
        ToolRouterParams {
            mcp_tools,
            deferred_mcp_tools,
            unavailable_called_tools,
            parallel_mcp_server_names,
            discoverable_tools,
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
        },
    )))
}

#[derive(Debug)]
pub(crate) struct SamplingRequestResult {
    pub(crate) needs_follow_up: bool,
    pub(crate) last_agent_message: Option<String>,
    pub(crate) non_tool_response_items: Vec<ResponseItem>,
}

/// Ephemeral per-response state for streaming a single proposed plan.
/// This is intentionally not persisted or stored in session/state since it
/// only exists while a response is actively streaming. The final plan text
/// is extracted from the completed assistant message.
/// Tracks a single proposed plan item across a streaming response.
struct ProposedPlanItemState {
    item_id: String,
    started: bool,
    completed: bool,
}

/// Aggregated state used only while streaming a plan-mode response.
/// Includes per-item parsers, deferred agent message bookkeeping, and the plan item lifecycle.
struct PlanModeStreamState {
    /// Agent message items started by the model but deferred until we see non-plan text.
    pending_agent_message_items: HashMap<String, TurnItem>,
    /// Agent message items whose start notification has been emitted.
    started_agent_message_items: HashSet<String>,
    /// Leading whitespace buffered until we see non-whitespace text for an item.
    leading_whitespace_by_item: HashMap<String, String>,
    /// Tracks plan item lifecycle while streaming plan output.
    plan_item_state: ProposedPlanItemState,
}

impl PlanModeStreamState {
    fn new(turn_id: &str) -> Self {
        Self {
            pending_agent_message_items: HashMap::new(),
            started_agent_message_items: HashSet::new(),
            leading_whitespace_by_item: HashMap::new(),
            plan_item_state: ProposedPlanItemState::new(turn_id),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct AssistantMessageStreamParsers {
    plan_mode: bool,
    parsers_by_item: HashMap<String, AssistantTextStreamParser>,
}

type ParsedAssistantTextDelta = AssistantTextChunk;

impl AssistantMessageStreamParsers {
    pub(super) fn new(plan_mode: bool) -> Self {
        Self {
            plan_mode,
            parsers_by_item: HashMap::new(),
        }
    }

    fn parser_mut(&mut self, item_id: &str) -> &mut AssistantTextStreamParser {
        let plan_mode = self.plan_mode;
        self.parsers_by_item
            .entry(item_id.to_string())
            .or_insert_with(|| AssistantTextStreamParser::new(plan_mode))
    }

    pub(super) fn seed_item_text(&mut self, item_id: &str, text: &str) -> ParsedAssistantTextDelta {
        if text.is_empty() {
            return ParsedAssistantTextDelta::default();
        }
        self.parser_mut(item_id).push_str(text)
    }

    pub(super) fn parse_delta(&mut self, item_id: &str, delta: &str) -> ParsedAssistantTextDelta {
        self.parser_mut(item_id).push_str(delta)
    }

    pub(super) fn finish_item(&mut self, item_id: &str) -> ParsedAssistantTextDelta {
        let Some(mut parser) = self.parsers_by_item.remove(item_id) else {
            return ParsedAssistantTextDelta::default();
        };
        parser.finish()
    }

    fn drain_finished(&mut self) -> Vec<(String, ParsedAssistantTextDelta)> {
        let parsers_by_item = std::mem::take(&mut self.parsers_by_item);
        parsers_by_item
            .into_iter()
            .map(|(item_id, mut parser)| (item_id, parser.finish()))
            .collect()
    }
}

impl ProposedPlanItemState {
    fn new(turn_id: &str) -> Self {
        Self {
            item_id: format!("{turn_id}-plan"),
            started: false,
            completed: false,
        }
    }

    async fn start(&mut self, sess: &Session, turn_context: &TurnContext) {
        if self.started || self.completed {
            return;
        }
        self.started = true;
        let item = TurnItem::Plan(PlanItem {
            id: self.item_id.clone(),
            text: String::new(),
        });
        sess.emit_turn_item_started(turn_context, &item).await;
    }

    async fn push_delta(&mut self, sess: &Session, turn_context: &TurnContext, delta: &str) {
        if self.completed {
            return;
        }
        if delta.is_empty() {
            return;
        }
        let event = PlanDeltaEvent {
            thread_id: sess.conversation_id.to_string(),
            turn_id: turn_context.sub_id.clone(),
            item_id: self.item_id.clone(),
            delta: delta.to_string(),
        };
        sess.send_event(turn_context, EventMsg::PlanDelta(event))
            .await;
    }

    async fn complete_with_text(
        &mut self,
        sess: &Session,
        turn_context: &TurnContext,
        text: String,
    ) {
        if self.completed || !self.started {
            return;
        }
        self.completed = true;
        let item = TurnItem::Plan(PlanItem {
            id: self.item_id.clone(),
            text,
        });
        sess.emit_turn_item_completed(turn_context, item).await;
    }
}

/// In plan mode we defer agent message starts until the parser emits non-plan
/// text. The parser buffers each line until it can rule out a tag prefix, so
/// plan-only outputs never show up as empty assistant messages.
async fn maybe_emit_pending_agent_message_start(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
) {
    if state.started_agent_message_items.contains(item_id) {
        return;
    }
    if let Some(item) = state.pending_agent_message_items.remove(item_id) {
        sess.emit_turn_item_started(turn_context, &item).await;
        state
            .started_agent_message_items
            .insert(item_id.to_string());
    }
}

/// Agent messages are text-only today; concatenate all text entries.
fn agent_message_text(item: &codex_protocol::items::AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

pub(super) fn realtime_text_for_event(msg: &EventMsg) -> Option<String> {
    match msg {
        EventMsg::AgentMessage(event) => Some(event.message.clone()),
        EventMsg::ItemCompleted(event) => match &event.item {
            TurnItem::AgentMessage(item) => Some(agent_message_text(item)),
            _ => None,
        },
        EventMsg::Error(_)
        | EventMsg::Warning(_)
        | EventMsg::RealtimeConversationStarted(_)
        | EventMsg::RealtimeConversationSdp(_)
        | EventMsg::RealtimeConversationRealtime(_)
        | EventMsg::RealtimeConversationClosed(_)
        | EventMsg::ModelReroute(_)
        | EventMsg::ContextCompacted(_)
        | EventMsg::ThreadRolledBack(_)
        | EventMsg::TurnStarted(_)
        | EventMsg::TurnComplete(_)
        | EventMsg::TokenCount(_)
        | EventMsg::UserMessage(_)
        | EventMsg::AgentMessageDelta(_)
        | EventMsg::AgentReasoning(_)
        | EventMsg::AgentReasoningDelta(_)
        | EventMsg::AgentReasoningRawContent(_)
        | EventMsg::AgentReasoningRawContentDelta(_)
        | EventMsg::AgentReasoningSectionBreak(_)
        | EventMsg::SessionConfigured(_)
        | EventMsg::ThreadNameUpdated(_)
        | EventMsg::McpStartupUpdate(_)
        | EventMsg::McpStartupComplete(_)
        | EventMsg::McpToolCallBegin(_)
        | EventMsg::McpToolCallEnd(_)
        | EventMsg::WebSearchBegin(_)
        | EventMsg::WebSearchEnd(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::ExecCommandOutputDelta(_)
        | EventMsg::TerminalInteraction(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::PatchApplyBegin(_)
        | EventMsg::PatchApplyUpdated(_)
        | EventMsg::PatchApplyEnd(_)
        | EventMsg::ViewImageToolCall(_)
        | EventMsg::ImageGenerationBegin(_)
        | EventMsg::ImageGenerationEnd(_)
        | EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::RequestUserInput(_)
        | EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_)
        | EventMsg::GuardianAssessment(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::DeprecationNotice(_)
        | EventMsg::BackgroundEvent(_)
        | EventMsg::UndoStarted(_)
        | EventMsg::UndoCompleted(_)
        | EventMsg::StreamError(_)
        | EventMsg::TurnDiff(_)
        | EventMsg::GetHistoryEntryResponse(_)
        | EventMsg::McpListToolsResponse(_)
        | EventMsg::ListSkillsResponse(_)
        | EventMsg::RealtimeConversationListVoicesResponse(_)
        | EventMsg::SkillsUpdateAvailable
        | EventMsg::PlanUpdate(_)
        | EventMsg::TurnAborted(_)
        | EventMsg::ShutdownComplete
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_)
        | EventMsg::RawResponseItem(_)
        | EventMsg::ItemStarted(_)
        | EventMsg::HookStarted(_)
        | EventMsg::HookCompleted(_)
        | EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::CollabAgentSpawnBegin(_)
        | EventMsg::CollabAgentSpawnEnd(_)
        | EventMsg::CollabAgentInteractionBegin(_)
        | EventMsg::CollabAgentInteractionEnd(_)
        | EventMsg::CollabWaitingBegin(_)
        | EventMsg::CollabWaitingEnd(_)
        | EventMsg::CollabCloseBegin(_)
        | EventMsg::CollabCloseEnd(_)
        | EventMsg::CollabResumeBegin(_)
        | EventMsg::CollabResumeEnd(_) => None,
    }
}

/// Split the stream into normal assistant text vs. proposed plan content.
/// Normal text becomes AgentMessage deltas; plan content becomes PlanDelta +
/// TurnItem::Plan.
async fn handle_plan_segments(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
    segments: Vec<ProposedPlanSegment>,
) {
    for segment in segments {
        match segment {
            ProposedPlanSegment::Normal(delta) => {
                if delta.is_empty() {
                    continue;
                }
                let has_non_whitespace = delta.chars().any(|ch| !ch.is_whitespace());
                if !has_non_whitespace && !state.started_agent_message_items.contains(item_id) {
                    let entry = state
                        .leading_whitespace_by_item
                        .entry(item_id.to_string())
                        .or_default();
                    entry.push_str(&delta);
                    continue;
                }
                let delta = if !state.started_agent_message_items.contains(item_id) {
                    if let Some(prefix) = state.leading_whitespace_by_item.remove(item_id) {
                        format!("{prefix}{delta}")
                    } else {
                        delta
                    }
                } else {
                    delta
                };
                maybe_emit_pending_agent_message_start(sess, turn_context, state, item_id).await;

                let event = AgentMessageContentDeltaEvent {
                    thread_id: sess.conversation_id.to_string(),
                    turn_id: turn_context.sub_id.clone(),
                    item_id: item_id.to_string(),
                    delta,
                };
                sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
                    .await;
            }
            ProposedPlanSegment::ProposedPlanStart => {
                if !state.plan_item_state.completed {
                    state.plan_item_state.start(sess, turn_context).await;
                }
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) => {
                if !state.plan_item_state.completed {
                    if !state.plan_item_state.started {
                        state.plan_item_state.start(sess, turn_context).await;
                    }
                    state
                        .plan_item_state
                        .push_delta(sess, turn_context, &delta)
                        .await;
                }
            }
            ProposedPlanSegment::ProposedPlanEnd => {}
        }
    }
}

async fn emit_streamed_assistant_text_delta(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    item_id: &str,
    parsed: ParsedAssistantTextDelta,
) {
    if parsed.is_empty() {
        return;
    }
    if !parsed.citations.is_empty() {
        // Citation extraction is intentionally local for now; we strip citations from display text
        // but do not yet surface them in protocol events.
        let _citations = parsed.citations;
    }
    if let Some(state) = plan_mode_state {
        if !parsed.plan_segments.is_empty() {
            handle_plan_segments(sess, turn_context, state, item_id, parsed.plan_segments).await;
        }
        return;
    }
    if parsed.visible_text.is_empty() {
        return;
    }
    let event = AgentMessageContentDeltaEvent {
        thread_id: sess.conversation_id.to_string(),
        turn_id: turn_context.sub_id.clone(),
        item_id: item_id.to_string(),
        delta: parsed.visible_text,
    };
    sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
        .await;
}

/// Flush buffered assistant text parser state when an assistant message item ends.
async fn flush_assistant_text_segments_for_item(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
    item_id: &str,
) {
    let parsed = parsers.finish_item(item_id);
    emit_streamed_assistant_text_delta(sess, turn_context, plan_mode_state, item_id, parsed).await;
}

/// Flush any remaining buffered assistant text parser state at response completion.
async fn flush_assistant_text_segments_all(
    sess: &Session,
    turn_context: &TurnContext,
    mut plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
) {
    for (item_id, parsed) in parsers.drain_finished() {
        emit_streamed_assistant_text_delta(
            sess,
            turn_context,
            plan_mode_state.as_deref_mut(),
            &item_id,
            parsed,
        )
        .await;
    }
}

/// Emit completion for plan items by parsing the finalized assistant message.
async fn maybe_complete_plan_item_from_message(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item: &ResponseItem,
) {
    if let ResponseItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(chunk);
            }
        }
        if let Some(plan_text) = extract_proposed_plan_text(&text) {
            let (plan_text, _citations) = strip_citations(&plan_text);
            if !state.plan_item_state.started {
                state.plan_item_state.start(sess, turn_context).await;
            }
            state
                .plan_item_state
                .complete_with_text(sess, turn_context, plan_text)
                .await;
        }
    }
}

/// Emit a completed agent message in plan mode, respecting deferred starts.
async fn emit_agent_message_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    agent_message: codex_protocol::items::AgentMessageItem,
    state: &mut PlanModeStreamState,
) {
    let agent_message_id = agent_message.id.clone();
    let text = agent_message_text(&agent_message);
    if text.trim().is_empty() {
        state.pending_agent_message_items.remove(&agent_message_id);
        state.started_agent_message_items.remove(&agent_message_id);
        return;
    }

    maybe_emit_pending_agent_message_start(sess, turn_context, state, &agent_message_id).await;

    if !state
        .started_agent_message_items
        .contains(&agent_message_id)
    {
        let start_item = state
            .pending_agent_message_items
            .remove(&agent_message_id)
            .unwrap_or_else(|| {
                TurnItem::AgentMessage(codex_protocol::items::AgentMessageItem {
                    id: agent_message_id.clone(),
                    content: Vec::new(),
                    phase: None,
                    memory_citation: None,
                })
            });
        sess.emit_turn_item_started(turn_context, &start_item).await;
        state
            .started_agent_message_items
            .insert(agent_message_id.clone());
    }

    sess.emit_turn_item_completed(turn_context, TurnItem::AgentMessage(agent_message))
        .await;
    state.started_agent_message_items.remove(&agent_message_id);
}

/// Emit completion for a plan-mode turn item, handling agent messages specially.
async fn emit_turn_item_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    turn_item: TurnItem,
    previously_active_item: Option<&TurnItem>,
    state: &mut PlanModeStreamState,
) {
    match turn_item {
        TurnItem::AgentMessage(agent_message) => {
            emit_agent_message_in_plan_mode(sess, turn_context, agent_message, state).await;
        }
        _ => {
            if previously_active_item.is_none() {
                sess.emit_turn_item_started(turn_context, &turn_item).await;
            }
            sess.emit_turn_item_completed(turn_context, turn_item).await;
        }
    }
}

/// Handle a completed assistant response item in plan mode, returning true if handled.
async fn handle_assistant_item_done_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    item: &ResponseItem,
    state: &mut PlanModeStreamState,
    previously_active_item: Option<&TurnItem>,
    last_agent_message: &mut Option<String>,
    execution_mode: SamplingExecutionMode,
) -> bool {
    if let ResponseItem::Message { role, .. } = item
        && role == "assistant"
    {
        maybe_complete_plan_item_from_message(sess, turn_context, state, item).await;

        if let Some(turn_item) =
            handle_non_tool_response_item(sess, turn_context, item, /*plan_mode*/ true).await
        {
            emit_turn_item_in_plan_mode(
                sess,
                turn_context,
                turn_item,
                previously_active_item,
                state,
            )
            .await;
        }

        record_completed_response_item(sess, turn_context, item, execution_mode).await;
        if let Some(agent_message) = last_assistant_message_from_item(item, /*plan_mode*/ true) {
            *last_agent_message = Some(agent_message);
        }
        return true;
    }
    false
}

async fn drain_in_flight(
    in_flight: &mut FuturesOrdered<BoxFuture<'static, CodexResult<ResponseInputItem>>>,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) -> CodexResult<()> {
    while let Some(res) = in_flight.next().await {
        match res {
            Ok(response_input) => {
                let response_item = response_input.into();
                sess.record_conversation_items(&turn_context, std::slice::from_ref(&response_item))
                    .await;
                mark_thread_memory_mode_polluted_if_external_context(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &response_item,
                )
                .await;
            }
            Err(err) => {
                error_or_panic(format!("in-flight tool future failed during drain: {err}"));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug
    )
)]
async fn try_run_sampling_request(
    tool_runtime: ToolCallRuntime,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    turn_metadata_header: Option<&str>,
    turn_diff_tracker: SharedTurnDiffTracker,
    server_model_warning_emitted_for_turn: &mut bool,
    prompt: &Prompt,
    cancellation_token: CancellationToken,
    execution_mode: SamplingExecutionMode,
) -> CodexResult<SamplingRequestResult> {
    let auth_mode = sess
        .services
        .auth_manager
        .auth_mode()
        .map_err(|error| CodexErr::Io(error.into_io_error()))?;
    feedback_tags!(
        model = turn_context.model_info.slug.clone(),
        approval_policy = turn_context.approval_policy.value(),
        sandbox_policy = turn_context.sandbox_policy.get(),
        effort = turn_context.reasoning_effort,
        auth_mode = auth_mode,
        features = sess.features.enabled_features(),
    );
    let mut stream = client_session
        .stream(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier,
            turn_metadata_header,
        )
        .instrument(trace_span!("stream_request"))
        .or_cancel(&cancellation_token)
        .await??;
    let mut in_flight: FuturesOrdered<BoxFuture<'static, CodexResult<ResponseInputItem>>> =
        FuturesOrdered::new();
    let mut needs_follow_up = false;
    let mut last_agent_message: Option<String> = None;
    let mut non_tool_response_items: Vec<ResponseItem> = Vec::new();
    let mut active_item: Option<TurnItem> = None;
    let mut active_tool_argument_diff_consumer: Option<(
        String,
        Box<dyn ToolArgumentDiffConsumer>,
    )> = None;
    let mut should_emit_turn_diff = false;
    let plan_mode = turn_context.collaboration_mode.mode == ModeKind::Plan;
    let mut assistant_message_stream_parsers = AssistantMessageStreamParsers::new(plan_mode);
    let mut plan_mode_state = plan_mode.then(|| PlanModeStreamState::new(&turn_context.sub_id));
    let receiving_span = trace_span!("receiving_stream");
    let outcome: CodexResult<SamplingRequestResult> = loop {
        let handle_responses = trace_span!(
            parent: &receiving_span,
            "handle_responses",
            otel.name = field::Empty,
            tool_name = field::Empty,
            from = field::Empty,
        );

        let event = match stream
            .next()
            .instrument(trace_span!(parent: &handle_responses, "receiving"))
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(event) => event,
            Err(codex_async_utils::CancelErr::Cancelled) => break Err(CodexErr::TurnAborted),
        };

        let event = match event {
            Some(Ok(event)) => event,
            Some(Err(err)) => break Err(err),
            None => {
                break Err(CodexErr::Stream(
                    "stream closed before response.completed".into(),
                    None,
                ));
            }
        };

        sess.services
            .session_telemetry
            .record_responses(&handle_responses, &event);
        record_turn_ttft_metric(&turn_context, &event).await;

        match event {
            ResponseEvent::Created => {}
            ResponseEvent::OutputItemDone(item) => {
                active_tool_argument_diff_consumer = None;
                let previously_active_item = active_item.take();
                if let Some(previous) = previously_active_item.as_ref()
                    && matches!(previous, TurnItem::AgentMessage(_))
                {
                    let item_id = previous.id();
                    flush_assistant_text_segments_for_item(
                        &sess,
                        &turn_context,
                        plan_mode_state.as_mut(),
                        &mut assistant_message_stream_parsers,
                        &item_id,
                    )
                    .await;
                }
                if let Some(state) = plan_mode_state.as_mut()
                    && handle_assistant_item_done_in_plan_mode(
                        &sess,
                        &turn_context,
                        &item,
                        state,
                        previously_active_item.as_ref(),
                        &mut last_agent_message,
                        execution_mode,
                    )
                    .await
                {
                    continue;
                }

                let mut ctx = HandleOutputCtx {
                    sess: sess.clone(),
                    turn_context: turn_context.clone(),
                    tool_runtime: tool_runtime.clone(),
                    cancellation_token: cancellation_token.child_token(),
                    execution_mode,
                };

                let preempt_for_mailbox_mail = match &item {
                    ResponseItem::Message { role, phase, .. } => {
                        role == "assistant" && matches!(phase, Some(MessagePhase::Commentary))
                    }
                    ResponseItem::Reasoning { .. } => true,
                    ResponseItem::LocalShellCall { .. }
                    | ResponseItem::FunctionCall { .. }
                    | ResponseItem::ToolSearchCall { .. }
                    | ResponseItem::FunctionCallOutput { .. }
                    | ResponseItem::CustomToolCall { .. }
                    | ResponseItem::CustomToolCallOutput { .. }
                    | ResponseItem::ToolSearchOutput { .. }
                    | ResponseItem::WebSearchCall { .. }
                    | ResponseItem::ImageGenerationCall { .. }
                    | ResponseItem::GhostSnapshot { .. }
                    | ResponseItem::Compaction { .. }
                    | ResponseItem::Other => false,
                };
                let completed_non_tool_item = matches!(
                    &item,
                    ResponseItem::Message { .. }
                        | ResponseItem::Reasoning { .. }
                        | ResponseItem::GhostSnapshot { .. }
                        | ResponseItem::Compaction { .. }
                        | ResponseItem::Other
                )
                .then(|| item.clone());

                let output_result =
                    match handle_output_item_done(&mut ctx, item, previously_active_item)
                        .instrument(handle_responses)
                        .await
                    {
                        Ok(output_result) => output_result,
                        Err(err) => break Err(err),
                    };
                let tool_future = output_result.tool_future;
                let has_tool_future = tool_future.is_some();
                if let Some(tool_future) = tool_future {
                    in_flight.push_back(tool_future);
                }
                if let Some(agent_message) = output_result.last_agent_message {
                    last_agent_message = Some(agent_message);
                }
                if !has_tool_future && let Some(item) = completed_non_tool_item {
                    non_tool_response_items.push(item);
                }
                needs_follow_up |= output_result.needs_follow_up;
                // todo: remove before stabilizing multi-agent v2
                if preempt_for_mailbox_mail && sess.mailbox_rx.lock().await.has_pending() {
                    break Ok(SamplingRequestResult {
                        needs_follow_up: true,
                        last_agent_message,
                        non_tool_response_items,
                    });
                }
            }
            ResponseEvent::OutputItemAdded(item) => {
                if let ResponseItem::CustomToolCall { call_id, name, .. } = &item {
                    let tool_name = ToolName::plain(name.as_str());
                    active_tool_argument_diff_consumer = tool_runtime
                        .create_diff_consumer(&tool_name)
                        .map(|consumer| (call_id.clone(), consumer));
                } else if matches!(&item, ResponseItem::FunctionCall { .. }) {
                    active_tool_argument_diff_consumer = None;
                }
                if let Some(turn_item) = handle_non_tool_response_item(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &item,
                    plan_mode,
                )
                .await
                {
                    let mut turn_item = turn_item;
                    let mut seeded_parsed: Option<ParsedAssistantTextDelta> = None;
                    let mut seeded_item_id: Option<String> = None;
                    if matches!(turn_item, TurnItem::AgentMessage(_))
                        && let Some(raw_text) = raw_assistant_output_text_from_item(&item)
                    {
                        let item_id = turn_item.id();
                        let mut seeded =
                            assistant_message_stream_parsers.seed_item_text(&item_id, &raw_text);
                        if let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                            agent_message.content =
                                vec![codex_protocol::items::AgentMessageContent::Text {
                                    text: if plan_mode {
                                        String::new()
                                    } else {
                                        std::mem::take(&mut seeded.visible_text)
                                    },
                                }];
                        }
                        seeded_parsed = plan_mode.then_some(seeded);
                        seeded_item_id = Some(item_id);
                    }
                    if let Some(state) = plan_mode_state.as_mut()
                        && matches!(turn_item, TurnItem::AgentMessage(_))
                    {
                        let item_id = turn_item.id();
                        state
                            .pending_agent_message_items
                            .insert(item_id, turn_item.clone());
                    } else {
                        sess.emit_turn_item_started(&turn_context, &turn_item).await;
                    }
                    if let (Some(state), Some(item_id), Some(parsed)) = (
                        plan_mode_state.as_mut(),
                        seeded_item_id.as_deref(),
                        seeded_parsed,
                    ) {
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            Some(state),
                            item_id,
                            parsed,
                        )
                        .await;
                    }
                    active_item = Some(turn_item);
                }
            }
            ResponseEvent::ServerModel(server_model) => {
                if !*server_model_warning_emitted_for_turn
                    && sess
                        .maybe_warn_on_server_model_mismatch(&turn_context, server_model)
                        .await
                {
                    *server_model_warning_emitted_for_turn = true;
                }
            }
            ResponseEvent::ServerReasoningIncluded(included) => {
                sess.set_server_reasoning_included(included).await;
            }
            ResponseEvent::RateLimits(snapshot) => {
                // Update internal state with latest rate limits, but defer sending until
                // token usage is available to avoid duplicate TokenCount events.
                sess.update_rate_limits(&turn_context, snapshot).await;
            }
            ResponseEvent::ModelsEtag(etag) => {
                // Update internal state with latest models etag
                sess.services.models_manager.refresh_if_new_etag(etag).await;
            }
            ResponseEvent::Completed {
                response_id: _,
                token_usage,
            } => {
                flush_assistant_text_segments_all(
                    &sess,
                    &turn_context,
                    plan_mode_state.as_mut(),
                    &mut assistant_message_stream_parsers,
                )
                .await;
                sess.update_token_usage_info(&turn_context, token_usage.as_ref())
                    .await;
                should_emit_turn_diff = true;

                break Ok(SamplingRequestResult {
                    needs_follow_up,
                    last_agent_message,
                    non_tool_response_items,
                });
            }
            ResponseEvent::OutputTextDelta(delta) => {
                // In review child threads, suppress assistant text deltas; the
                // UI will show a selection popup from the final ReviewOutput.
                if let Some(active) = active_item.as_ref() {
                    let item_id = active.id();
                    if matches!(active, TurnItem::AgentMessage(_)) {
                        let parsed = assistant_message_stream_parsers.parse_delta(&item_id, &delta);
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            plan_mode_state.as_mut(),
                            &item_id,
                            parsed,
                        )
                        .await;
                    } else {
                        let event = AgentMessageContentDeltaEvent {
                            thread_id: sess.conversation_id.to_string(),
                            turn_id: turn_context.sub_id.clone(),
                            item_id,
                            delta,
                        };
                        sess.send_event(&turn_context, EventMsg::AgentMessageContentDelta(event))
                            .await;
                    }
                } else {
                    error_or_panic("OutputTextDelta without active item".to_string());
                }
            }
            ResponseEvent::ToolCallInputDelta {
                item_id: _,
                call_id,
                delta,
            } => {
                let Some((active_call_id, consumer)) = active_tool_argument_diff_consumer.as_mut()
                else {
                    continue;
                };
                let call_id = match call_id {
                    Some(call_id) if call_id.as_str() != active_call_id.as_str() => continue,
                    Some(call_id) => call_id,
                    None => active_call_id.clone(),
                };
                if let Some(event) = consumer.consume_diff(turn_context.as_ref(), call_id, &delta) {
                    sess.send_event(&turn_context, event).await;
                }
            }
            ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    let event = ReasoningContentDeltaEvent {
                        thread_id: sess.conversation_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        summary_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningSummaryDelta without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
                if let Some(active) = active_item.as_ref() {
                    let event =
                        EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {
                            item_id: active.id(),
                            summary_index,
                        });
                    sess.send_event(&turn_context, event).await;
                } else {
                    error_or_panic("ReasoningSummaryPartAdded without active item".to_string());
                }
            }
            ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    let event = ReasoningRawContentDeltaEvent {
                        thread_id: sess.conversation_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        content_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningRawContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningRawContentDelta without active item".to_string());
                }
            }
        }
    };

    flush_assistant_text_segments_all(
        &sess,
        &turn_context,
        plan_mode_state.as_mut(),
        &mut assistant_message_stream_parsers,
    )
    .await;

    drain_in_flight(&mut in_flight, sess.clone(), turn_context.clone()).await?;

    if cancellation_token.is_cancelled() {
        return Err(CodexErr::TurnAborted);
    }

    if should_emit_turn_diff {
        let unified_diff = {
            let mut tracker = turn_diff_tracker.lock().await;
            tracker.get_unified_diff()
        };
        if let Ok(Some(unified_diff)) = unified_diff {
            let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
            sess.clone().send_event(&turn_context, msg).await;
        }
    }

    outcome
}

pub(crate) fn get_last_assistant_message_from_turn(responses: &[ResponseItem]) -> Option<String> {
    for item in responses.iter().rev() {
        if let Some(message) = last_assistant_message_from_item(item, /*plan_mode*/ false) {
            return Some(message);
        }
    }
    None
}
