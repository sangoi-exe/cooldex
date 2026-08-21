//! Handles reply-bearing turn-input operations.
//!
//! This is the one place Core decides whether submitted input starts a turn,
//! steers an active turn, or is rejected. It replies after that decision; it
//! does not wait for user-prompt hooks, updating the in-memory model context,
//! rollout persistence, or sampling.
//!
//! Persistent thread settings apply on Started and Steered. Turn start
//! options only apply on Started.

use super::SteerInputError;
use super::TurnInput;
use super::session::Session;
use super::session::SessionSettingsUpdate;
use super::thread_settings;
use super::turn_context::TurnContext;
use crate::tasks::MailboxParentProvenance;
use crate::tasks::RegularTask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::TurnInput as SubmittedTurnInput;
use codex_protocol::turn_input::TurnInputMode;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::turn_input::TurnStartOptions;
use codex_protocol::user_input::UserInput;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

#[cfg(test)]
#[path = "turn_input_tests.rs"]
mod tests;

/// Thread settings and start-only options prepared before Core knows whether
/// turn input starts or steers.
///
/// Thread settings are validated up front but only applied after Core accepts
/// the input. Start-only options are only consumed by `apply_started`.
struct PreparedTurnInputSettings {
    thread_settings_update: Option<SessionSettingsUpdate>,
    start_options: TurnStartOptions,
}

impl PreparedTurnInputSettings {
    /// Validates turn-input settings without applying them so rejected input
    /// leaves the thread unchanged.
    async fn prepare(
        session: &Session,
        thread_settings: ThreadSettingsOverrides,
        start_options: TurnStartOptions,
    ) -> CodexResult<Self> {
        let thread_settings_update = if thread_settings == ThreadSettingsOverrides::default() {
            None
        } else {
            let updates = thread_settings::prepare_update(session, thread_settings).await;
            session
                .preview_settings(&updates)
                .await
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
            Some(updates)
        };
        Ok(Self {
            thread_settings_update,
            start_options,
        })
    }

    fn required_active_final_output_json_schema(&self) -> Option<&Value> {
        self.start_options.final_output_json_schema.as_ref()
    }

    fn would_enter_plan_mode(&self) -> bool {
        self.thread_settings_update
            .as_ref()
            .and_then(|updates| updates.collaboration_mode.as_ref())
            .is_some_and(|collaboration_mode| collaboration_mode.mode == ModeKind::Plan)
    }

    /// Applies persistent settings and start-only options before creating a
    /// new turn context.
    async fn apply_started(
        self,
        session: &Arc<Session>,
        submission_id: String,
    ) -> CodexResult<Arc<TurnContext>> {
        let TurnStartOptions {
            final_output_json_schema,
            parent_turn_id,
            root_turn_id,
        } = self.start_options;
        let emit_thread_settings_applied = self.thread_settings_update.is_some();
        let mut updates = self.thread_settings_update.unwrap_or_default();
        updates.final_output_json_schema = Some(final_output_json_schema);

        // new_turn_with_sub_id already emits an error event when settings are invalid.
        let turn_context = session
            .new_turn_with_sub_id(submission_id.clone(), updates)
            .await?;
        if emit_thread_settings_applied {
            thread_settings::emit_applied(session, submission_id).await;
        }
        if let Some(parent_turn_id) = parent_turn_id {
            turn_context
                .turn_metadata_state
                .set_parent_turn_id(parent_turn_id);
        }
        if let Some(root_turn_id) = root_turn_id {
            turn_context
                .turn_metadata_state
                .set_root_turn_id(root_turn_id);
        }
        Ok(turn_context)
    }

    /// Applies only persistent settings after steering succeeds. The active
    /// turn keeps its existing context; subsequent turns see the update.
    async fn apply_steered(self, session: &Session, submission_id: String) -> CodexResult<()> {
        let Some(thread_settings_update) = self.thread_settings_update else {
            return Ok(());
        };
        thread_settings::apply_update(session, submission_id, thread_settings_update)
            .await
            .map_err(|error| CodexErr::InvalidRequest(error.to_string()))
    }
}

pub(super) async fn handle(
    session: &Arc<Session>,
    request: TurnInputRequest,
    mode: TurnInputMode,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    match mode {
        TurnInputMode::StartOrSteer => start_or_steer(session, request, submission_id).await,
        TurnInputMode::StartIfIdle => {
            start_if_idle(session, request, submission_id, /*is_recovery*/ false).await
        }
        TurnInputMode::Steer { expected_turn_id } => {
            steer(session, request, expected_turn_id, submission_id).await
        }
    }
}

pub(super) async fn handle_recovery(
    session: &Arc<Session>,
    thread_settings: ThreadSettingsOverrides,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let request = TurnInputRequest::user_input(Vec::new()).with_thread_settings(thread_settings);
    start_if_idle(session, request, submission_id, /*is_recovery*/ true).await
}

async fn start_or_steer(
    session: &Arc<Session>,
    request: TurnInputRequest,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        ..
    } = request;
    let SubmittedTurnInput::UserInput {
        content: items,
        client_id,
    } = input
    else {
        return Err(CodexErr::InvalidRequest(
            "only user input can steer a turn".to_string(),
        ));
    };
    let can_start_root_turn = start.parent_turn_id.is_none() && start.root_turn_id.is_none();
    let incoming_root_turn_id = start
        .parent_turn_id
        .as_ref()
        .map(|_| start.root_turn_id.clone());
    let settings = PreparedTurnInputSettings::prepare(session, thread_settings, start).await?;
    if active_turn_output_schema_mismatch(
        session,
        /*expected_turn_id*/ None,
        settings.required_active_final_output_json_schema(),
    )
    .await
    {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ActiveTurnOutputSchemaMismatch,
        });
    }
    match session
        .steer_input(
            items.clone(),
            additional_context.clone(),
            /*expected_turn_id*/ None,
            client_id.clone(),
            responsesapi_client_metadata.clone(),
        )
        .await
    {
        Ok(turn_id) => {
            maybe_mark_root_turn_ambiguous(session, &turn_id, incoming_root_turn_id).await;
            settings.apply_steered(session, submission_id).await?;
            Ok(TurnInputSubmission::Steered { turn_id })
        }
        Err(SteerInputError::NoActiveTurn(_)) => {
            let turn_context = settings
                .apply_started(session, submission_id.clone())
                .await?;
            if can_start_root_turn
                && !items.is_empty()
                && turn_context
                    .turn_metadata_state
                    .can_start_root_turn(&turn_context.session_source)
            {
                turn_context
                    .turn_metadata_state
                    .set_root_turn_id(submission_id.clone());
            }
            if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
                turn_context
                    .turn_metadata_state
                    .set_responsesapi_client_metadata(responsesapi_client_metadata);
            }
            session
                .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
                .await;
            turn_context.session_telemetry.user_prompt(&items);
            let mut task_input = merge_additional_context_input(session, additional_context).await;
            if !items.is_empty() {
                task_input.push(TurnInput::UserInput {
                    content: items,
                    client_id,
                });
            }
            session
                .spawn_task(turn_context, task_input, RegularTask::new())
                .await;
            Ok(TurnInputSubmission::Started {
                turn_id: submission_id,
            })
        }
        Err(error) => Ok(TurnInputSubmission::NotSubmitted {
            reason: map_steer_rejection(error)?,
        }),
    }
}

async fn start_if_idle(
    session: &Arc<Session>,
    request: TurnInputRequest,
    submission_id: String,
    is_recovery: bool,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        ..
    } = request;
    let has_user_input = has_nonempty_user_input(&input);
    let is_automatic_idle_work = !has_user_input && !is_recovery;
    let can_start_root_turn = start.parent_turn_id.is_none() && start.root_turn_id.is_none();
    if session.input_queue.has_trigger_turn_mailbox_items().await {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        });
    }
    // Empty non-recovery starts are automatic wakeups, not explicit user requests.
    // Do not let them start a Plan turn.
    if is_automatic_idle_work && session.collaboration_mode().await.mode == ModeKind::Plan {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        });
    }

    let claim = {
        let mut slot = session.active_turn.lock().await;
        if !slot.is_idle() {
            return Ok(TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::NotIdle,
            });
        }
        slot.claim_start(submission_id.clone())
            .map_err(|error| CodexErr::Fatal(format!("turn-slot invariant violation: {error}")))?
    };

    if session.input_queue.has_trigger_turn_mailbox_items().await {
        session.cancel_claimed_start(&claim).await;
        session.maybe_start_turn_for_pending_work().await;
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        });
    }

    let settings = match PreparedTurnInputSettings::prepare(session, thread_settings, start).await {
        Ok(settings) => settings,
        Err(error) => {
            session.cancel_claimed_start(&claim).await;
            return Err(error);
        }
    };
    // Automatic work must not use persistent settings to start a turn
    // whose effective collaboration mode is Plan.
    if is_automatic_idle_work && settings.would_enter_plan_mode() {
        session.cancel_claimed_start(&claim).await;
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        });
    }

    let turn_context = match settings.apply_started(session, submission_id.clone()).await {
        Ok(turn_context) => turn_context,
        Err(error) => {
            session.cancel_claimed_start(&claim).await;
            return Err(error);
        }
    };
    if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
        turn_context
            .turn_metadata_state
            .set_responsesapi_client_metadata(responsesapi_client_metadata);
    }
    if has_user_input
        && can_start_root_turn
        && turn_context
            .turn_metadata_state
            .can_start_root_turn(&turn_context.session_source)
    {
        turn_context
            .turn_metadata_state
            .set_root_turn_id(submission_id.clone());
    }
    session
        .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
        .await;

    let mut task_input = merge_additional_context_input(session, additional_context).await;
    if has_user_input {
        session.clear_connector_selection().await;
        if let SubmittedTurnInput::UserInput { content, .. } = &input {
            turn_context.session_telemetry.user_prompt(content);
        }
        task_input.push(pending_turn_input(input));
    } else if is_automatic_idle_work && !matches!(&input, SubmittedTurnInput::UserInput { .. }) {
        // Automatic response-item work still needs to be queued, but an empty
        // user-input request should start sampling without adding a message.
        session
            .input_queue
            .extend_pending_input_for_turn_state(
                claim.turn_state.as_ref(),
                vec![pending_turn_input(input)],
            )
            .await;
    }
    session
        .start_claimed_regular_task_with_options(
            claim,
            turn_context,
            task_input,
            None,
            MailboxParentProvenance::Ignore,
        )
        .await?;
    Ok(TurnInputSubmission::Started {
        turn_id: submission_id,
    })
}

async fn steer(
    session: &Arc<Session>,
    request: TurnInputRequest,
    expected_turn_id: String,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        ..
    } = request;
    let SubmittedTurnInput::UserInput {
        content: items,
        client_id,
    } = input
    else {
        return Err(CodexErr::InvalidRequest(
            "only user input can steer a turn".to_string(),
        ));
    };
    let incoming_root_turn_id = start
        .parent_turn_id
        .as_ref()
        .map(|_| start.root_turn_id.clone());
    let settings = PreparedTurnInputSettings::prepare(session, thread_settings, start).await?;
    if active_turn_output_schema_mismatch(
        session,
        Some(expected_turn_id.as_str()),
        settings.required_active_final_output_json_schema(),
    )
    .await
    {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ActiveTurnOutputSchemaMismatch,
        });
    }
    match session
        .steer_input(
            items,
            additional_context,
            Some(expected_turn_id.as_str()),
            client_id,
            responsesapi_client_metadata,
        )
        .await
    {
        Ok(turn_id) => {
            maybe_mark_root_turn_ambiguous(session, &turn_id, incoming_root_turn_id).await;
            settings.apply_steered(session, submission_id).await?;
            Ok(TurnInputSubmission::Steered { turn_id })
        }
        Err(error) => Ok(TurnInputSubmission::NotSubmitted {
            reason: map_steer_rejection(error)?,
        }),
    }
}

impl Session {
    pub(crate) async fn route_realtime_text_input(self: &Arc<Self>, text: String) {
        let submission_id = Uuid::now_v7().to_string();
        let submission = handle(
            self,
            TurnInputRequest::user_input(vec![UserInput::Text {
                text,
                text_elements: Vec::new(),
            }]),
            TurnInputMode::StartOrSteer,
            submission_id.clone(),
        )
        .await;
        match submission {
            Ok(TurnInputSubmission::Started { .. } | TurnInputSubmission::Steered { .. }) => {}
            Ok(TurnInputSubmission::NotSubmitted { reason }) => {
                self.send_event_raw(Event {
                    id: submission_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("failed to submit turn input: {reason:?}"),
                        codex_error_info: Some(CodexErrorInfo::BadRequest),
                    }),
                })
                .await;
            }
            Err(error) => {
                self.send_event_raw(Event {
                    id: submission_id,
                    msg: EventMsg::Error(error.to_error_event(/*message_prefix*/ None)),
                })
                .await;
            }
        }
    }
}

fn has_nonempty_user_input(input: &SubmittedTurnInput) -> bool {
    matches!(input, SubmittedTurnInput::UserInput { content, .. } if !content.is_empty())
}

fn map_steer_rejection(error: SteerInputError) -> CodexResult<NotSubmittedReason> {
    match error {
        SteerInputError::NoActiveTurn(_) => Ok(NotSubmittedReason::NoActiveTurn),
        SteerInputError::ExpectedTurnMismatch { expected, actual } => {
            Ok(NotSubmittedReason::ExpectedTurnMismatch { expected, actual })
        }
        SteerInputError::ActiveTurnNotSteerable { turn_kind } => {
            Ok(NotSubmittedReason::ActiveTurnNotSteerable { turn_kind })
        }
        SteerInputError::EmptyInput => Ok(NotSubmittedReason::EmptyInput),
        SteerInputError::TurnSlotInvariant(message) => Err(CodexErr::Fatal(format!(
            "turn-slot invariant violation: {message}"
        ))),
    }
}

async fn active_turn_output_schema_mismatch(
    session: &Session,
    expected_turn_id: Option<&str>,
    required_final_output_json_schema: Option<&Value>,
) -> bool {
    let Some(required_schema) = required_final_output_json_schema else {
        return false;
    };
    let active_turn_context = {
        let slot = session.active_turn.lock().await;
        let Some(active_task) = slot.running_task() else {
            return false;
        };
        if let Some(expected_turn_id) = expected_turn_id
            && active_task.turn_context.sub_id != expected_turn_id
        {
            return false;
        }
        Arc::clone(&active_task.turn_context)
    };
    active_turn_context.final_output_json_schema.as_ref() != Some(required_schema)
}

async fn maybe_mark_root_turn_ambiguous(
    session: &Session,
    turn_id: &str,
    incoming_root_turn_id: Option<Option<String>>,
) {
    let Some(incoming_root_turn_id) = incoming_root_turn_id else {
        return;
    };
    let active_turn_context = {
        let slot = session.active_turn.lock().await;
        let Some(active_task) = slot.running_task() else {
            return;
        };
        if active_task.turn_context.sub_id != turn_id {
            return;
        }
        Arc::clone(&active_task.turn_context)
    };
    if active_turn_context.turn_metadata_state.root_turn_id() != incoming_root_turn_id {
        active_turn_context
            .turn_metadata_state
            .mark_root_turn_ambiguous();
    }
}

async fn merge_additional_context_input(
    session: &Session,
    additional_context: BTreeMap<String, AdditionalContextEntry>,
) -> Vec<TurnInput> {
    let additional_context_input = {
        let mut state = session.state.lock().await;
        state.additional_context.merge(additional_context)
    };
    additional_context_input
        .into_iter()
        .map(ResponseItem::from)
        .map(|item| session.annotate_client_response_item(item))
        .map(TurnInput::ResponseItem)
        .collect()
}

fn pending_turn_input(input: SubmittedTurnInput) -> TurnInput {
    match input {
        SubmittedTurnInput::UserInput { content, client_id } => {
            TurnInput::UserInput { content, client_id }
        }
        SubmittedTurnInput::ResponseItem(item) => TurnInput::ResponseItem(item.into()),
        SubmittedTurnInput::InterAgentCommunication(communication) => {
            TurnInput::InterAgentCommunication(communication)
        }
    }
}
