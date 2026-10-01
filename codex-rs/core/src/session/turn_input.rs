//! Handles reply-bearing turn-input operations.
//!
//! This is the one place Core decides whether submitted input starts a turn,
//! steers an active turn, or is rejected. It replies after that decision; it
//! does not wait for user-prompt hooks, updating the in-memory model context,
//! rollout persistence, or sampling.
//!
//! Persistent thread settings apply on Started and Steered. Turn start
//! options only update turn context on Started; input provenance follows each request.
//! Host shutdown admission is checked before reserving or starting a new turn.
//! Parent-delegated subagent input bypasses drain; automatic starts remain gated.
//! Realtime drain refusals are returned to the fanout for ordered session teardown.

use super::SteerInputError;
use super::TurnInput;
use super::session::Session;
use super::session::SessionConfiguration;
use super::session::SessionSettingsUpdate;
use super::thread_settings;
use super::turn_context::NewTurnContextOptions;
use super::turn_context::TurnContext;
use crate::tasks::RegularTask;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_history::UserInputOrigin;
use codex_protocol::config_types::ModeKind;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::NonSteerableTurnKind;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
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
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[cfg(test)]
#[path = "turn_input_tests.rs"]
mod tests;

/// Why input is starting a turn; shared by admission and input delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnStartKind {
    User,
    Automatic,
    Recovery,
}

impl TurnStartKind {
    fn permits_mode(self, mode: ModeKind) -> bool {
        match self {
            Self::User | Self::Recovery => true,
            Self::Automatic => mode != ModeKind::Plan,
        }
    }

    /// Automatic work may neither leave an existing Plan mode nor enter it.
    fn permits_settings(
        self,
        current: &SessionConfiguration,
        proposed: &SessionConfiguration,
    ) -> bool {
        self.permits_mode(current.step_settings.collaboration_mode.mode)
            && self.permits_mode(proposed.step_settings.collaboration_mode.mode)
    }
}

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
            let updates = thread_settings::prepare_update(thread_settings);
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

    /// Applies persistent settings and start-only options before creating a
    /// new turn context. Returns `None` if admission rejects the candidate,
    /// without committing its settings.
    async fn apply_started(
        self,
        session: &Arc<Session>,
        submission_id: String,
        kind: TurnStartKind,
    ) -> CodexResult<Option<Arc<TurnContext>>> {
        let TurnStartOptions {
            turn_trigger,
            final_output_json_schema,
            service_tier,
            parent_turn_id,
            root_turn_id,
            cyber_access_program,
        } = self.start_options;
        let emit_thread_settings_applied = self.thread_settings_update.is_some();
        let _settings_guard = if emit_thread_settings_applied {
            Some(thread_settings::acquire_persistence_lock(session).await)
        } else {
            None
        };
        let mut updates = self.thread_settings_update.unwrap_or_default();
        updates.service_tier_for_turn = service_tier;

        let options = NewTurnContextOptions {
            final_output_json_schema,
            cyber_access_program,
        };
        let turn_context = match kind {
            TurnStartKind::User | TurnStartKind::Recovery => Some(
                session
                    .new_turn_with_sub_id(submission_id.clone(), updates, options)
                    .await?,
            ),
            TurnStartKind::Automatic => {
                session
                    .new_turn_with_sub_id_if(
                        submission_id.clone(),
                        updates,
                        options,
                        |current, proposed| kind.permits_settings(current, proposed),
                    )
                    .await?
            }
        };
        let Some((turn_context, settings_snapshot)) = turn_context else {
            return Ok(None);
        };
        if let Some(turn_trigger) = turn_trigger {
            turn_context
                .turn_metadata_state
                .set_turn_trigger(turn_trigger);
        }
        if emit_thread_settings_applied {
            thread_settings::emit_applied(session, submission_id, settings_snapshot).await;
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
        Ok(Some(turn_context))
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
            let kind = match &request.input {
                SubmittedTurnInput::UserInput { content, .. } if !content.is_empty() => {
                    TurnStartKind::User
                }
                SubmittedTurnInput::UserInput { .. }
                | SubmittedTurnInput::ResponseItem(_)
                | SubmittedTurnInput::InterAgentCommunication(_) => TurnStartKind::Automatic,
            };
            start_if_idle(
                session,
                request,
                submission_id,
                kind,
                /*expected_previous_turn_id*/ None,
            )
            .await
        }
        TurnInputMode::ContinueIfIdle {
            expected_previous_turn_id,
        } => {
            if !matches!(&request.input, SubmittedTurnInput::ResponseItem(_)) {
                return Err(CodexErr::InvalidRequest(
                    "continuation requires internal response input".to_string(),
                ));
            }
            start_if_idle(
                session,
                request,
                submission_id,
                TurnStartKind::Recovery,
                Some(expected_previous_turn_id),
            )
            .await
        }
        TurnInputMode::Steer { expected_turn_id } => {
            steer(session, request, expected_turn_id, submission_id).await
        }
    }
}

pub(super) async fn handle_recovery(
    session: &Arc<Session>,
    thread_settings: ThreadSettingsOverrides,
    start_options: TurnStartOptions,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let request = TurnInputRequest::user_input(Vec::new())
        .with_thread_settings(thread_settings)
        .on_start(TurnStartOptions {
            turn_trigger: Some("retry".to_string()),
            ..start_options
        });
    start_if_idle(
        session,
        request,
        submission_id,
        TurnStartKind::Recovery,
        /*expected_previous_turn_id*/ None,
    )
    .await
}

// Merge-safety anchor: start-or-steer projects typed SteerInputError results; only NoActiveTurn
// falls through to regular-turn creation.
async fn start_or_steer(
    session: &Arc<Session>,
    request: TurnInputRequest,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        mut input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        ..
    } = request;
    let origin = UserInputOrigin::from_turn_trigger(start.turn_trigger.as_deref());
    let has_explicit_input = match &input {
        SubmittedTurnInput::UserInput { content, .. } => !content.is_empty(),
        SubmittedTurnInput::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id: None,
            ..
        }) => true,
        _ => {
            return Err(CodexErr::InvalidRequest(
                "only user input or standalone function-call outputs can start or steer a turn"
                    .to_string(),
            ));
        }
    };
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
            &mut input,
            additional_context.clone(),
            /*expected_turn_id*/ None,
            settings.required_active_final_output_json_schema(),
            responsesapi_client_metadata.clone(),
            origin,
            incoming_root_turn_id,
        )
        .await
    {
        Ok(turn_id) => {
            settings.apply_steered(session, submission_id).await?;
            Ok(TurnInputSubmission::Steered { turn_id })
        }
        Err(SteerInputError::NoActiveTurn(retained_input)) => {
            if let SubmittedTurnInput::UserInput { content, .. } = &mut input {
                *content = retained_input;
            }
            // MAv1 sends explicit input to spawned agents as part of an existing
            // parent's work. Client RPCs are gated separately by the host.
            let is_delegated_input = settings.start_options.parent_turn_id.is_some()
                && matches!(
                    session
                        .state
                        .lock()
                        .await
                        .session_configuration
                        .session_source,
                    SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
                );
            let _admission = if is_delegated_input {
                None
            } else {
                let Some(admission) = session.services.extensions.admit_turn_start() else {
                    return Ok(TurnInputSubmission::NotSubmitted {
                        reason: NotSubmittedReason::ServerDraining,
                    });
                };
                Some(admission)
            };
            let Some(turn_context) = settings
                .apply_started(session, submission_id.clone(), TurnStartKind::User)
                .await?
            else {
                unreachable!("explicit user input can enter Plan mode");
            };
            if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
                turn_context
                    .turn_metadata_state
                    .set_responsesapi_client_metadata(responsesapi_client_metadata);
            }
            session
                .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
                .await;
            if let SubmittedTurnInput::UserInput { content, .. } = &input {
                turn_context.session_telemetry.user_prompt(content);
            }
            let mut task_input = merge_additional_context_input(session, additional_context).await;
            if has_explicit_input {
                task_input
                    .push(pending_turn_input(session, input, &turn_context.sub_id, origin).await);
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

// Merge-safety anchor: idle start reserves the sole ActiveTurn, rechecks admission, and cancels
// an unopened reservation while returning a rejection reason.
async fn start_if_idle(
    session: &Arc<Session>,
    request: TurnInputRequest,
    submission_id: String,
    kind: TurnStartKind,
    expected_previous_turn_id: Option<String>,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        ..
    } = request;
    let origin = UserInputOrigin::from_turn_trigger(start.turn_trigger.as_deref());
    if session.input_queue.has_trigger_turn_mailbox_items().await {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        });
    }
    // Preserve current-Plan rejection before reservation and settings errors.
    // The commit-time decision also checks the proposed mode.
    if kind == TurnStartKind::Automatic
        && !kind.permits_mode(session.collaboration_mode().await.mode)
    {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        });
    }

    let _admission = session.services.extensions.admit_turn_start();
    // A one-shot review delegate completes its already-running parent's work.
    // Its explicit input carries parent lineage; automatic starts do not qualify.
    if _admission.is_none()
        && !(kind == TurnStartKind::User
            && start.parent_turn_id.is_some()
            && matches!(
                session
                    .state
                    .lock()
                    .await
                    .session_configuration
                    .session_source,
                SessionSource::SubAgent(SubAgentSource::Review)
            ))
    {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ServerDraining,
        });
    }

    if let Some(expected) = expected_previous_turn_id
        && session.state.lock().await.last_started_turn_id.as_ref() != Some(&expected)
    {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::Superseded,
        });
    }
    let Some(turn_state) = session.reserve_turn_start(&submission_id).await else {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        });
    };

    if session.input_queue.has_trigger_turn_mailbox_items().await {
        session.cancel_reserved_turn_start(&submission_id).await;
        session.maybe_start_turn_for_pending_work().await;
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        });
    }

    let settings = match PreparedTurnInputSettings::prepare(session, thread_settings, start).await {
        Ok(settings) => settings,
        Err(error) => {
            session.cancel_reserved_turn_start(&submission_id).await;
            return Err(error);
        }
    };
    let turn_context = match settings
        .apply_started(session, submission_id.clone(), kind)
        .await
    {
        Ok(Some(turn_context)) => turn_context,
        Ok(None) => {
            session.cancel_reserved_turn_start(&submission_id).await;
            return Ok(TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::PlanMode,
            });
        }
        Err(error) => {
            session.cancel_reserved_turn_start(&submission_id).await;
            return Err(error);
        }
    };
    if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
        turn_context
            .turn_metadata_state
            .set_responsesapi_client_metadata(responsesapi_client_metadata);
    }
    session
        .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
        .await;

    let mut task_input = merge_additional_context_input(session, additional_context).await;
    match kind {
        TurnStartKind::User => {
            session.clear_connector_selection().await;
            if let SubmittedTurnInput::UserInput { content, .. } = &input {
                turn_context.session_telemetry.user_prompt(content);
            }
            task_input.push(pending_turn_input(session, input, &turn_context.sub_id, origin).await);
        }
        TurnStartKind::Automatic | TurnStartKind::Recovery => {
            // Empty automatic user input resumes sampling without a new message.
            if !matches!(&input, SubmittedTurnInput::UserInput { .. }) {
                session
                    .input_queue
                    .extend_pending_input_for_turn_state(
                        turn_state.as_ref(),
                        vec![
                            pending_turn_input(session, input, &turn_context.sub_id, origin).await,
                        ],
                    )
                    .await;
            }
        }
    }
    if !session
        .start_task(turn_context, task_input, RegularTask::new())
        .await
    {
        session.cancel_reserved_turn_start(&submission_id).await;
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        });
    }
    Ok(TurnInputSubmission::Started {
        turn_id: submission_id,
    })
}

// Merge-safety anchor: explicit steering retains expected-turn routing and typed SteerInputError
// projection without falling back to a new task.
async fn steer(
    session: &Arc<Session>,
    request: TurnInputRequest,
    expected_turn_id: String,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        mut input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        ..
    } = request;
    let origin = UserInputOrigin::from_turn_trigger(start.turn_trigger.as_deref());
    if !matches!(&input, SubmittedTurnInput::UserInput { .. }) {
        return Err(CodexErr::InvalidRequest(
            "only user input can steer a turn".to_string(),
        ));
    }
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
            &mut input,
            additional_context,
            Some(expected_turn_id.as_str()),
            settings.required_active_final_output_json_schema(),
            responsesapi_client_metadata,
            origin,
            incoming_root_turn_id,
        )
        .await
    {
        Ok(turn_id) => {
            settings.apply_steered(session, submission_id).await?;
            Ok(TurnInputSubmission::Steered { turn_id })
        }
        Err(error) => Ok(TurnInputSubmission::NotSubmitted {
            reason: map_steer_rejection(error)?,
        }),
    }
}

impl Session {
    /// Called under the active-turn lock before running any task or lifecycle callback.
    pub(crate) async fn record_started_turn(&self, turn_id: &str) {
        self.state.lock().await.last_started_turn_id = Some(turn_id.to_string());
    }

    pub(crate) async fn route_realtime_text_input(
        self: &Arc<Self>,
        text: String,
    ) -> Result<(), &'static str> {
        let submission_id = Uuid::now_v7().to_string();
        let submission = handle(
            self,
            TurnInputRequest::user_input(vec![UserInput::Text {
                text,
                text_elements: Vec::new(),
            }])
            .on_start(TurnStartOptions {
                turn_trigger: Some("realtime".to_string()),
                ..Default::default()
            }),
            TurnInputMode::StartOrSteer,
            submission_id.clone(),
        )
        .await;
        match submission {
            Ok(TurnInputSubmission::Started { .. } | TurnInputSubmission::Steered { .. }) => {}
            Ok(TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::ServerDraining,
            }) => {
                return Err("Server is draining; retry the turn after reconnecting");
            }
            Ok(TurnInputSubmission::NotSubmitted { reason }) => {
                self.send_event_raw(Event {
                    id: submission_id,
                    msg: EventMsg::Error(ErrorEvent {
                        misalignment: None,
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
        Ok(())
    }
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
        SteerInputError::ActiveTurnOutputSchemaMismatch => {
            Ok(NotSubmittedReason::ActiveTurnOutputSchemaMismatch)
        }
        SteerInputError::EmptyInput => Ok(NotSubmittedReason::EmptyInput),
    }
}

// Merge-safety anchor: schema preflight and steering share turn_input as their canonical owner while retaining the finishing-slot and root-provenance contracts.
async fn active_turn_output_schema_mismatch(
    session: &Session,
    expected_turn_id: Option<&str>,
    required_final_output_json_schema: Option<&Value>,
) -> bool {
    let Some(required_schema) = required_final_output_json_schema else {
        return false;
    };
    let active_turn_context = {
        let active_turn = session.active_turn.lock().await;
        let Some(active_task) = active_turn.as_ref().and_then(|turn| turn.task.as_ref()) else {
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

impl Session {
    /// Injects additional user input or a standalone function-call output into the active task.
    ///
    /// Reservations are not steerable: only an installed ActiveTurn task may receive input.
    #[expect(
        clippy::too_many_arguments,
        reason = "steering keeps active-turn routing, schema, client metadata, and input provenance explicit"
    )]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active task identity and its pending-input state must be captured atomically"
    )]
    pub(crate) async fn steer_input(
        &self,
        input: &mut SubmittedTurnInput,
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        expected_turn_id: Option<&str>,
        required_final_output_json_schema: Option<&Value>,
        responsesapi_client_metadata: Option<HashMap<String, String>>,
        origin: UserInputOrigin,
        incoming_root_turn_id: Option<Option<String>>,
    ) -> Result<String, SteerInputError> {
        let retained_input = match input {
            SubmittedTurnInput::UserInput { content, .. } => content.clone(),
            _ => Vec::new(),
        };
        loop {
            let active = self.active_turn.lock().await;
            let Some(active_turn) = active.as_ref() else {
                return Err(SteerInputError::NoActiveTurn(retained_input));
            };
            let Some(active_task) = active_turn.task.as_ref() else {
                return Err(SteerInputError::NoActiveTurn(retained_input));
            };
            // Merge-safety anchor: terminal slots remain non-admittable until their task
            // notifies retirement, so a fresh input cannot overtake the terminal event.
            if active_turn.finishing {
                let mut done = Box::pin(Arc::clone(&active_task.done).notified_owned());
                let _ = done.as_mut().enable();
                drop(active);
                done.await;
                continue;
            }
            let active_turn_id = active_task.turn_context.sub_id.clone();
            if let Some(expected_turn_id) = expected_turn_id
                && expected_turn_id != active_turn_id
            {
                return Err(SteerInputError::ExpectedTurnMismatch {
                    expected: expected_turn_id.to_string(),
                    actual: active_turn_id,
                });
            }
            match active_task.kind {
                crate::state::TaskKind::Regular => {}
                crate::state::TaskKind::Review => {
                    return Err(SteerInputError::ActiveTurnNotSteerable {
                        turn_kind: NonSteerableTurnKind::Review,
                    });
                }
                crate::state::TaskKind::Compact => {
                    return Err(SteerInputError::ActiveTurnNotSteerable {
                        turn_kind: NonSteerableTurnKind::Compact,
                    });
                }
            }
            if matches!(input, SubmittedTurnInput::UserInput { content, .. } if content.is_empty())
            {
                return Err(SteerInputError::EmptyInput);
            }
            if let Some(required_schema) = required_final_output_json_schema
                && active_task.turn_context.final_output_json_schema.as_ref()
                    != Some(required_schema)
            {
                return Err(SteerInputError::ActiveTurnOutputSchemaMismatch);
            }
            let (turn_input, client_user_message_id) = match input {
                SubmittedTurnInput::UserInput { content, client_id } => {
                    active_task
                        .turn_context
                        .session_telemetry
                        .user_prompt(content);
                    let client_id = client_id.clone();
                    (
                        TurnInput::UserInput {
                            content: std::mem::take(content),
                            client_id: client_id.clone(),
                            metadata: super::UserInputMetadata {
                                acceptance_order: Some(self.reserve_user_input_order().await),
                                origin,
                            },
                        },
                        client_id,
                    )
                }
                SubmittedTurnInput::ResponseItem(item)
                    if matches!(item, ResponseItem::FunctionCallOutput { call_id: None, .. }) =>
                {
                    (
                        pending_turn_input(
                            self,
                            SubmittedTurnInput::ResponseItem(item.clone()),
                            &active_turn_id,
                            origin,
                        )
                        .await,
                        None,
                    )
                }
                _ => return Err(SteerInputError::EmptyInput),
            };
            if active_task
                .turn_context
                .turn_metadata_state
                .root_turn_id()
                .is_none()
                && let Some(Some(incoming_root_turn_id)) = incoming_root_turn_id
            {
                active_task
                    .turn_context
                    .turn_metadata_state
                    .set_root_turn_id(incoming_root_turn_id);
            }
            let active_turn_context = Arc::clone(&active_task.turn_context);
            let turn_state = Arc::clone(&active_turn.turn_state);
            drop(active);
            return self
                .queue_turn_input(
                    active_turn_context,
                    turn_state,
                    turn_input,
                    additional_context,
                    client_user_message_id,
                    responsesapi_client_metadata,
                    retained_input,
                )
                .await;
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "queued-input delivery keeps active-turn context, state, metadata, and retained input explicit"
    )]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active-turn validation and queue insertion must remain atomic across queued delivery"
    )]
    async fn queue_turn_input(
        &self,
        active_turn_context: Arc<TurnContext>,
        turn_state: Arc<Mutex<crate::state::TurnState>>,
        input: TurnInput,
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        client_user_message_id: Option<String>,
        responsesapi_client_metadata: Option<HashMap<String, String>>,
        retained_input: Vec<UserInput>,
    ) -> Result<String, SteerInputError> {
        let additional_context_input = {
            let mut state = self.state.lock().await;
            state.additional_context.merge(additional_context)
        };
        let active = self.active_turn.lock().await;
        if let Some(active_task) = active
            .as_ref()
            .filter(|active_turn| active_turn.finishing)
            .and_then(|active_turn| active_turn.task.as_ref())
        {
            let mut done = Box::pin(Arc::clone(&active_task.done).notified_owned());
            let _ = done.as_mut().enable();
            drop(active);
            done.await;
            return Err(SteerInputError::NoActiveTurn(retained_input));
        }
        if !active.as_ref().is_some_and(|active_turn| {
            !active_turn.finishing
                && active_turn.task.as_ref().is_some_and(|task| {
                    Arc::ptr_eq(&task.turn_context, &active_turn_context)
                        && Arc::ptr_eq(&active_turn.turn_state, &turn_state)
                })
        }) {
            return Err(SteerInputError::NoActiveTurn(retained_input));
        }
        if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
            active_turn_context
                .turn_metadata_state
                .set_responsesapi_client_metadata(responsesapi_client_metadata);
        }
        let mut pending_input = additional_context_input
            .into_iter()
            .map(|item| self.annotate_client_response_item(item))
            .map(TurnInput::ResponseItem)
            .collect::<Vec<_>>();
        pending_input.push(input);
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                turn_state.as_ref(),
                pending_input,
            )
            .await;
        drop(active);
        if let Some(client_id) = client_user_message_id.as_deref() {
            self.pending_user_message_admissions
                .associate_steered_by_client_id(client_id, &active_turn_context.sub_id);
        }
        Ok(active_turn_context.sub_id.clone())
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
        .map(|item| session.annotate_client_response_item(item))
        .map(TurnInput::ResponseItem)
        .collect()
}

async fn pending_turn_input(
    session: &Session,
    input: SubmittedTurnInput,
    turn_id: &str,
    origin: UserInputOrigin,
) -> TurnInput {
    match input {
        SubmittedTurnInput::UserInput { content, client_id } => TurnInput::UserInput {
            content,
            client_id,
            metadata: super::UserInputMetadata {
                acceptance_order: Some(session.reserve_user_input_order().await),
                origin,
            },
        },
        SubmittedTurnInput::ResponseItem(mut item)
            if matches!(
                &item,
                ResponseItem::FunctionCallOutput { call_id: None, .. }
            ) =>
        {
            Session::assign_missing_response_item_id(&mut item);
            let metadata = if let Some(messages) = session
                .services
                .local_agent_runtime
                .capture_sender_user_messages(&item, session.thread_id, turn_id)
                .await
            {
                Some(CodexHarnessMetadata {
                    user_input_order: Some(session.reserve_user_input_order().await),
                    sender_user_messages: Some(Box::new(messages)),
                    ..Default::default()
                })
            } else {
                None
            };
            TurnInput::FunctionCallOutput(ResponseItemEnvelope { item, metadata })
        }
        SubmittedTurnInput::ResponseItem(item) => TurnInput::ResponseItem(item.into()),
        SubmittedTurnInput::InterAgentCommunication(communication) => {
            TurnInput::InterAgentCommunication(communication)
        }
    }
}
