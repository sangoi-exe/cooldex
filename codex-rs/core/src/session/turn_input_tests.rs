use super::*;
use crate::config::Constrained;
use crate::session::step_settings::StepSettingsUpdate;
use crate::session::tests::make_session_and_context;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskResult;
use crate::test_support::GatedModelsManager;
use codex_features::Feature;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::AgentPath;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::Settings;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelAccessPrograms;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AdditionalContextKind;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::NonSteerableTurnKind;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::turn_input::CyberAccessProgram;
use codex_protocol::turn_input::TurnInput as SubmittedTurnInput;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::test_codex::local_selections;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use test_case::test_case;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

struct ConfigRecorder(Arc<std::sync::Mutex<Vec<(AskForApproval, ApprovalsReviewer)>>>);

impl codex_extension_api::ConfigContributor<crate::config::Config> for ConfigRecorder {
    fn on_config_changed(
        &self,
        _session_store: &codex_extension_api::ExtensionData,
        _thread_store: &codex_extension_api::ExtensionData,
        _previous_config: &crate::config::Config,
        new_config: &crate::config::Config,
    ) {
        self.0.lock().expect("config records lock").push((
            new_config.permissions.approval_policy.value(),
            new_config.approvals_reviewer,
        ));
    }
}

fn admission_models(slugs: &[&str]) -> Vec<ModelInfo> {
    let template = codex_models_manager::bundled_models_response()
        .expect("bundled models")
        .models
        .into_iter()
        .find(|model| model.slug == "gpt-5.5")
        .expect("bundled gpt-5.5");
    slugs
        .iter()
        .map(|slug| ModelInfo {
            slug: (*slug).to_string(),
            ..template.clone()
        })
        .collect()
}

#[derive(Clone, Copy)]
struct NeverEndingTask {
    kind: TaskKind,
    listen_to_cancellation_token: bool,
}

impl SessionTask for NeverEndingTask {
    fn kind(&self) -> TaskKind {
        self.kind
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn_input_test"
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        _ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        if self.listen_to_cancellation_token {
            cancellation_token.cancelled().await;
            return Ok(Default::default());
        }
        loop {
            sleep(std::time::Duration::from_secs(60)).await;
        }
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

async fn submit_start_only(
    session: &Arc<Session>,
    input: SubmittedTurnInput,
) -> TurnInputSubmission {
    handle(
        session,
        TurnInputRequest::new(input),
        TurnInputMode::StartIfIdle,
        "test-submission".to_string(),
    )
    .await
    .expect("start-only submission should be valid")
}

async fn submit_steer_only(
    session: &Arc<Session>,
    input: Vec<UserInput>,
    expected_turn_id: &str,
) -> TurnInputSubmission {
    handle(
        session,
        TurnInputRequest::new(SubmittedTurnInput::UserInput {
            content: input,
            client_id: None,
        }),
        TurnInputMode::Steer {
            expected_turn_id: expected_turn_id.to_string(),
        },
        "test-submission".to_string(),
    )
    .await
    .expect("steer-only submission should be valid")
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "simulate an in-flight realtime append while checking input admission"
)]
async fn steering_does_not_wait_for_realtime_history() {
    let (mut session, turn_context) = make_session_and_context().await;
    session.realtime_history = Some(tokio::sync::Mutex::new(Default::default()));
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let history = session
        .realtime_history
        .as_ref()
        .expect("realtime history")
        .lock()
        .await;
    for mode in [
        TurnInputMode::StartOrSteer,
        TurnInputMode::Steer {
            expected_turn_id: turn_context.sub_id.clone(),
        },
    ] {
        let submission = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle(
                &session,
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: "steer without waiting for persistence".to_string(),
                    text_elements: Vec::new(),
                }]),
                mode,
                "steer-submission".to_string(),
            ),
        )
        .await
        .expect("steering must not wait for the realtime recorder")
        .expect("steering should succeed");
        assert_eq!(
            submission,
            TurnInputSubmission::Steered {
                root_turn_id: turn_context.sub_id.clone(),
                turn_id: turn_context.sub_id.clone()
            }
        );
    }
    drop(history);
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

// Merge-safety anchor: both steering input kinds release ActiveTurn before confirmed-delivery ordering waits, reject retired tasks without rerouting, and deliver unseen context exactly once on an accepted same-map retry.
#[test_case(false; "user_input")]
#[test_case(true; "standalone_function_output")]
#[tokio::test]
async fn steering_order_wait_allows_retirement_without_rerouting_input(standalone_output: bool) {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    let content = vec![UserInput::Text {
        text: "Keep this input with its admission.".to_owned(),
        text_elements: Vec::new(),
    }];
    let mut input = if standalone_output {
        SubmittedTurnInput::ResponseItem(
            serde_json::from_value(serde_json::json!({
                "type": "function_call_output",
                "id": "delivery-input",
                "name": "send_message_to_thread",
                "namespace": "codex_app",
                "output": "delegated work",
            }))
            .expect("recognized standalone delivery"),
        )
    } else {
        SubmittedTurnInput::UserInput {
            content: content.clone(),
            client_id: Some("original-client-message".to_owned()),
        }
    };
    let original_input = input.clone();
    let additional_context = BTreeMap::from([
        (
            "automation_info".to_owned(),
            AdditionalContextEntry {
                value: "unseen run".to_owned(),
                kind: AdditionalContextKind::Application,
                name: None,
            },
        ),
        (
            "browser_info".to_owned(),
            AdditionalContextEntry {
                value: "unseen tab".to_owned(),
                kind: AdditionalContextKind::Untrusted,
                name: None,
            },
        ),
    ]);
    let boundary = session
        .code_mode_message_tasks
        .communication_boundary
        .acquire()
        .await
        .expect("open communication boundary");
    // Queue confirmed delivery ahead of steering; interruption can then take persistence before the delivery writer does.
    let message = codex_history::RetainedUserMessage {
        origin: UserInputOrigin::User,
        turn_id: turn_context.sub_id.clone(),
        message_id: Some("confirmed-send".to_owned()),
        text: "May I publish?".to_owned(),
        complete: true,
        phase: None,
    };
    let (recording, _) = session.record_delivered_assistant_message(message.clone());
    // Production captures recognized standalone deliveries before steering, even without sender provenance.
    let sender_messages = capture_sender_user_messages(&session, &mut input).await;
    assert_eq!(sender_messages.is_some(), standalone_output);
    let mut steering = Box::pin(session.steer_input(
        &mut input,
        sender_messages.as_ref(),
        additional_context.clone(),
        Some(&turn_context.sub_id),
        /*required_final_output_json_schema*/ None,
        ResponsesApiClientMetadataUpdate {
            replacement: Some(HashMap::from([(
                "workspace_kind".to_owned(),
                "original".to_owned(),
            )])),
            ..Default::default()
        },
        UserInputMetadata {
            origin: UserInputOrigin::Heartbeat,
            ..Default::default()
        },
        /*incoming_root_turn_id*/ None,
    ));
    assert!(futures::poll!(steering.as_mut()).is_pending());
    let mut interruption = Box::pin(session.abort_turn_if_active(
        &turn_context.sub_id,
        TurnAbortReason::Interrupted,
        /*error*/ None,
    ));
    assert!(futures::poll!(interruption.as_mut()).is_pending());
    drop(boundary);
    let (aborted, recorded) =
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 5), async {
            tokio::join!(interruption, recording)
        })
        .await
        .expect("retirement and confirmed delivery must not deadlock with waiting steering");
    assert!(aborted);
    recorded.expect("confirmed delivery finishes despite retirement");

    // Reuse the turn ID deliberately: admission must check the captured context/state, not just a string ID.
    let replacement = session
        .new_turn_with_default_settings(
            turn_context.sub_id.clone(),
            NewTurnContextOptions::default(),
        )
        .await
        .expect("valid work context");
    session
        .spawn_task(
            Arc::clone(&replacement),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    assert_eq!(
        steering.await,
        Err(SteerInputError::NoActiveTurn(if standalone_output {
            Vec::new()
        } else {
            content.clone()
        }))
    );
    if standalone_output {
        assert_eq!(input, original_input);
    }
    assert!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0
            .is_empty()
    );
    assert_eq!(replacement.turn_metadata_state.workspace_kind(), None);
    let deliveries = session
        .clone_history()
        .await
        .retained_context()
        .ordered_entries()
        .filter_map(|(_, entry)| match entry {
            codex_history::RetainedContextEntry::AssistantMessage(message) => Some(message.clone()),
            codex_history::RetainedContextEntry::UserMessage(_)
            | codex_history::RetainedContextEntry::VerifiedAnswer(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(deliveries, vec![message]);

    // Retry the rejected body explicitly, then consume through the production queue boundary.
    // A second accepted same-map submission must not enqueue the fragments again.
    for retry in 0..2 {
        input = original_input.clone();
        let sender_messages = capture_sender_user_messages(&session, &mut input).await;
        assert_eq!(
            session
                .steer_input(
                    &mut input,
                    sender_messages.as_ref(),
                    additional_context.clone(),
                    Some(&replacement.sub_id),
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ Default::default(),
                    UserInputMetadata {
                        origin: UserInputOrigin::Heartbeat,
                        ..Default::default()
                    },
                    /*incoming_root_turn_id*/ None,
                )
                .await,
            Ok((replacement.sub_id.clone(), replacement.root_turn_id()))
        );
        let mut delivered = session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0;
        let delivered_input = match delivered.pop().expect("accepted original body") {
            TurnInput::UserInput {
                content,
                client_id,
                metadata,
            } => {
                assert_eq!(metadata.origin, UserInputOrigin::Heartbeat);
                assert!(metadata.acceptance_order.is_some());
                SubmittedTurnInput::UserInput { content, client_id }
            }
            TurnInput::FunctionCallOutput(envelope) => {
                SubmittedTurnInput::ResponseItem(envelope.item)
            }
            other => panic!("unexpected accepted body: {other:?}"),
        };
        assert_eq!(delivered_input, original_input);
        let delivered_context = delivered
            .into_iter()
            .map(|item| match item {
                TurnInput::ResponseItem(envelope) => envelope.item,
                other => panic!("unexpected additional context: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            delivered_context,
            if retry == 0 {
                [
                    (
                        "developer",
                        "automation_info",
                        "<automation_info>unseen run</automation_info>",
                    ),
                    (
                        "user",
                        "browser_info",
                        "<external_browser_info>unseen tab</external_browser_info>",
                    ),
                ]
                .into_iter()
                .map(|(role, key, text)| ResponseItem::Message {
                    status: None,
                    encrypted_content: None,
                    id: None,
                    role: role.to_owned(),
                    content: vec![ContentItem::InputText {
                        text: text.to_owned(),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: Some(
                        InternalChatMessageMetadataPassthrough {
                            content_item_kinds: Some(vec![ContentItemKind(format!(
                                "additional_content.{key}"
                            ))]),
                            content_item_metadata: Some(vec![ContentItemMetadata::harness()]),
                            ..Default::default()
                        },
                    ),
                })
                .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        );
    }
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

// Merge-safety anchor: the refreshed MCP runtime receives server elicitation authority changes.
#[tokio::test]
async fn accepted_input_applies_thread_settings() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let config = session.get_config().await;
    handle(
        &session,
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }])
        .with_thread_settings(ThreadSettingsOverrides {
            environments: Some(local_selections(config.cwd.clone()).into_requests()),
            approval_policy: Some(config.permissions.approval_policy.value()),
            approvals_reviewer: Some(codex_config::types::ApprovalsReviewer::AutoReview),
            sandbox_policy: Some(config.legacy_sandbox_policy()),
            summary: config.model_reasoning_summary,
            personality: config.personality,
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: turn_context.model_info().slug.clone(),
                    reasoning_effort: config.model_reasoning_effort.clone(),
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        }),
        TurnInputMode::StartOrSteer,
        "sub-1".to_string(),
    )
    .await
    .expect("submit user turn");

    {
        let state = session.state.lock().await;
        assert_eq!(
            state.session_configuration.step_settings.approvals_reviewer,
            codex_config::types::ApprovalsReviewer::AutoReview
        );
    }
    session.refresh_mcp_if_dirty().await;
    let binding = session
        .services
        .mcp_runtime
        .current_binding()
        .await
        .expect("refreshed runtime should be available");
    assert!(
        binding.config().approvals_reviewer == codex_config::types::ApprovalsReviewer::AutoReview,
        "server elicitation authority changes must reach the active MCP runtime"
    );
}

#[tokio::test]
async fn start_only_rejects_active_turn_without_injecting() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let input = SubmittedTurnInput::ResponseItem(user_message("synthetic idle input"));
    let submission = submit_start_only(&session, input).await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        },
        submission
    );
    assert_eq!(
        Vec::<TurnInput>::new(),
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn recovery_rejects_active_turn_without_injecting_or_applying_settings() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let original_approval_policy = session
        .get_config()
        .await
        .permissions
        .approval_policy
        .value();
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let submission = handle_recovery(
        &session,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        },
        TurnStartOptions::default(),
        "recovered-turn".to_string(),
    )
    .await
    .expect("recovery should return a typed rejection");

    assert_eq!(
        submission,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        }
    );
    assert_eq!(
        session
            .get_config()
            .await
            .permissions
            .approval_policy
            .value(),
        original_approval_policy
    );
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        Vec::<TurnInput>::new()
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn start_only_rejects_current_plan_before_validating_settings() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let default_mode = session.collaboration_mode().await;
    {
        let mut state = session.state.lock().await;
        let settings = Arc::make_mut(&mut state.session_configuration.step_settings);
        settings.collaboration_mode.mode = ModeKind::Plan;
        settings.approval_policy = Constrained::allow_only(AskForApproval::OnRequest);
    }
    let desired_settings = session.thread_settings_snapshot().await;
    let invalid_override = ThreadSettingsOverrides {
        collaboration_mode: Some(default_mode.clone()),
        approval_policy: Some(AskForApproval::Never),
        ..Default::default()
    };

    // Current Plan takes precedence even when the request would leave Plan or
    // fail settings validation. Nothing has been reserved or applied yet.
    let submission = handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::ResponseItem(user_message(
            "synthetic idle input",
        )))
        .with_thread_settings(invalid_override.clone()),
        TurnInputMode::StartIfIdle,
        "automatic-plan-submission".to_string(),
    )
    .await
    .expect("current Plan must reject before settings validation");
    assert_eq!(
        submission,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        }
    );
    assert_eq!(session.thread_settings_snapshot().await, desired_settings);
    assert!(session.active_turn.lock().await.is_none());

    session
        .update_settings(SessionSettingsUpdate {
            step_settings: StepSettingsUpdate {
                collaboration_mode: Some(default_mode),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("explicit settings may leave Plan mode");
    let desired_settings = session.thread_settings_snapshot().await;
    let result = handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::ResponseItem(user_message(
            "invalid automatic input",
        )))
        .with_thread_settings(invalid_override),
        TurnInputMode::StartIfIdle,
        "invalid-automatic-submission".to_string(),
    )
    .await;
    let error = result.expect_err("invalid automatic settings must be rejected");
    assert!(matches!(
        error.details(),
        CodexErrorDetails::InvalidRequest(_)
    ));
    assert_eq!(session.thread_settings_snapshot().await, desired_settings);
    assert!(session.active_turn.lock().await.is_none());
    assert_eq!(
        Vec::<TurnInput>::new(),
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0
    );
}

#[tokio::test]
async fn prepared_user_updates_merge_with_settings_at_turn_start() {
    for (requested, intervening, gated_model) in [
        (
            ThreadSettingsOverrides {
                effort: Some(Some(ReasoningEffort::High)),
                ..Default::default()
            },
            ThreadSettingsOverrides {
                model: Some("gpt-5.2".to_string()),
                ..Default::default()
            },
            "gpt-5.4",
        ),
        (
            ThreadSettingsOverrides {
                model: Some("gpt-5.2".to_string()),
                ..Default::default()
            },
            ThreadSettingsOverrides {
                effort: Some(Some(ReasoningEffort::High)),
                ..Default::default()
            },
            "gpt-5.2",
        ),
    ] {
        let (mut session, _turn_context, _rx) = make_session_and_context_with_rx().await;
        let initial = CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: "gpt-5.4".to_string(),
                reasoning_effort: Some(ReasoningEffort::Low),
                developer_instructions: None,
            },
        };
        session
            .update_settings(thread_settings::prepare_update(
                ThreadSettingsOverrides {
                    collaboration_mode: Some(initial.clone()),
                    ..Default::default()
                },
                &session.services.selected_capability_roots,
            ))
            .await
            .expect("set initial model and effort");
        let requested = ThreadSettingsOverrides {
            permission_profile: Some(PermissionProfile::read_only()),
            active_permission_profile: Some(ActivePermissionProfile::read_only()),
            ..requested
        };
        let prepared =
            PreparedTurnInputSettings::prepare(&session, requested, TurnStartOptions::default())
                .await
                .expect("prepare partial protocol update");
        let (models, lookup) =
            GatedModelsManager::new(admission_models(&["gpt-5.4", "gpt-5.2"]), gated_model);
        Arc::get_mut(&mut session)
            .expect("unique test session")
            .services
            .models_manager = models;
        let starting = prepared.apply_started(
            &session,
            "sparse-user-start".to_string(),
            TurnStartKind::User,
        );
        tokio::pin!(starting);
        tokio::select! {
            _ = &mut starting => panic!("admission must wait for model metadata"),
            () = lookup.wait_until_blocked() => {}
        }
        session
            .update_settings(thread_settings::prepare_update(
                intervening,
                &session.services.selected_capability_roots,
            ))
            .await
            .expect("commit intervening settings");
        lookup.release();
        let turn_context = tokio::time::timeout(std::time::Duration::from_secs(10), starting)
            .await
            .expect("equivalent permission patches must not repeat preparation indefinitely")
            .expect("apply prepared settings")
            .expect("user start is permitted");
        let expected = initial.with_updates(
            Some("gpt-5.2".to_string()),
            Some(Some(ReasoningEffort::High)),
            /*developer_instructions*/ None,
        );
        assert_eq!(session.collaboration_mode().await, expected);
        assert_eq!(turn_context.collaboration_mode(), expected);
        assert_eq!(
            turn_context.initial_settings.selected_collaboration_mode(),
            &expected
        );
        assert!(Arc::ptr_eq(
            &turn_context.initial_settings.model_info,
            turn_context.model_info(),
        ));
        assert_eq!(turn_context.model_info().slug, "gpt-5.2");
        assert_eq!(
            session
                .thread_settings_snapshot()
                .await
                .active_permission_profile,
            Some(ActivePermissionProfile::read_only())
        );
    }
}

#[tokio::test]
async fn admission_reprepares_model_metadata_when_personality_changes() {
    let (mut session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    {
        let mut state = session.state.lock().await;
        let configuration = &mut state.session_configuration;
        Arc::make_mut(&mut configuration.step_settings).personality = Some(Personality::Friendly);
        configuration.model_info_overrides.base_instructions = None;
    }
    let prepared = PreparedTurnInputSettings::prepare(
        &session,
        ThreadSettingsOverrides {
            effort: Some(Some(ReasoningEffort::High)),
            ..Default::default()
        },
        TurnStartOptions::default(),
    )
    .await
    .expect("preview sparse settings");
    let model = session.collaboration_mode().await.settings.model;
    let mut models = admission_models(&[&model]);
    models[0]
        .model_messages
        .get_or_insert_default()
        .instructions_template =
        Some("prefix\n# Personality\nfriendly\n# Other\nkeep\n".to_string());
    let (models, lookup) = GatedModelsManager::new(models, &model);
    Arc::get_mut(&mut session)
        .expect("unique test session")
        .services
        .models_manager = models;
    let starting = prepared.apply_started(
        &session,
        "personality-during-lookup".to_string(),
        TurnStartKind::User,
    );
    tokio::pin!(starting);
    tokio::select! {
        _ = &mut starting => panic!("admission must wait for model metadata"),
        () = lookup.wait_until_blocked() => {}
    }
    session
        .update_settings(SessionSettingsUpdate {
            step_settings: StepSettingsUpdate {
                personality: Some(Personality::None),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("change personality during admission lookup");
    lookup.release();
    let turn = starting
        .await
        .expect("admission should succeed")
        .expect("user starts are unconditional");
    assert_eq!(turn.initial_settings.personality(), Some(Personality::None));
    assert_eq!(
        turn.model_info()
            .model_messages
            .as_ref()
            .and_then(|messages| messages.instructions_template.as_deref()),
        Some("prefix\n# Other\nkeep\n")
    );
}

#[tokio::test]
async fn automatic_admission_uses_current_candidate_after_plan_preview() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let default_mode = session.collaboration_mode().await;
    let mut plan_mode = default_mode.clone();
    plan_mode.mode = ModeKind::Plan;
    session
        .update_settings(thread_settings::prepare_update(
            ThreadSettingsOverrides {
                collaboration_mode: Some(plan_mode),
                ..Default::default()
            },
            &session.services.selected_capability_roots,
        ))
        .await
        .expect("enter Plan after the initial admission check");
    let prepared = PreparedTurnInputSettings::prepare(
        &session,
        ThreadSettingsOverrides {
            effort: Some(Some(ReasoningEffort::High)),
            ..Default::default()
        },
        TurnStartOptions::default(),
    )
    .await
    .expect("validate the patch while the preview is Plan");
    session
        .update_settings(thread_settings::prepare_update(
            ThreadSettingsOverrides {
                collaboration_mode: Some(default_mode.clone()),
                ..Default::default()
            },
            &session.services.selected_capability_roots,
        ))
        .await
        .expect("leave Plan before atomic admission");

    let turn_context = prepared
        .apply_started(
            &session,
            "automatic-after-plan-preview".to_string(),
            TurnStartKind::Automatic,
        )
        .await
        .expect("automatic admission should succeed")
        .expect("current and proposed modes are both Default");
    let expected = default_mode.with_updates(
        /*model*/ None,
        Some(Some(ReasoningEffort::High)),
        /*developer_instructions*/ None,
    );
    assert_eq!(session.collaboration_mode().await, expected);
    assert_eq!(turn_context.collaboration_mode(), expected);
    assert_eq!(
        turn_context.initial_settings.selected_collaboration_mode(),
        &expected
    );
}

#[tokio::test]
async fn automatic_admission_rechecks_plan_mode_without_committing_sparse_settings() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    extensions.config_contributor(Arc::new(ConfigRecorder(Arc::clone(&records))));
    Arc::get_mut(&mut session)
        .expect("unique test session")
        .services
        .extensions = Arc::new(extensions.build());
    {
        let mut state = session.state.lock().await;
        let settings = Arc::make_mut(&mut state.session_configuration.step_settings);
        settings.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
        settings.approvals_reviewer = ApprovalsReviewer::User;
    }
    let original_environments = session.services.turn_environments.selections();
    let workspace = tempfile::tempdir().expect("create proposed workspace");
    let proposed_environments = local_selections(
        AbsolutePathBuf::try_from(workspace.path()).expect("absolute workspace path"),
    );
    assert_ne!(original_environments, proposed_environments.environments);
    let default_mode = session.collaboration_mode().await;
    let overrides = ThreadSettingsOverrides {
        model: Some("automatic-model-must-not-be-applied".to_string()),
        service_tier: Some(Some(ServiceTier::Fast.request_value().to_string())),
        environments: Some(proposed_environments.clone().into_requests()),
        approval_policy: Some(AskForApproval::Never),
        approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
        ..Default::default()
    };
    let default_override = ThreadSettingsOverrides {
        collaboration_mode: Some(default_mode.clone()),
        ..overrides.clone()
    };
    let prepared = PreparedTurnInputSettings::prepare(
        &session,
        overrides.clone(),
        TurnStartOptions::default(),
    )
    .await
    .expect("sparse settings should preview successfully");
    let prepared_default =
        PreparedTurnInputSettings::prepare(&session, default_override, TurnStartOptions::default())
            .await
            .expect("Default replacement should preview successfully");

    // Another settings writer changes the effective mode after preview. The
    // sparse patch must not commit in Plan, and a full Default replacement
    // must not let automatic work escape the now-current Plan configuration.
    let mut collaboration_mode = default_mode;
    collaboration_mode.mode = ModeKind::Plan;
    session
        .update_settings(SessionSettingsUpdate {
            step_settings: StepSettingsUpdate {
                collaboration_mode: Some(collaboration_mode),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("Plan mode should be allowed for explicit settings updates");
    let desired_settings = session.thread_settings_snapshot().await;
    records.lock().expect("config records lock").clear();
    // Keep the existing refresh worker from consuming invalidation while the
    // rejection and its positive control inspect MCP's dirty state.
    let _mcp_refresh = session
        .mcp_refresh
        .acquire()
        .await
        .expect("acquire MCP refresh gate");
    session.mcp_refresh.claim();
    assert!(!session.mcp_refresh.is_pending());

    for (submission_id, prepared) in [
        ("automatic-after-preview", prepared),
        ("automatic-default-after-preview", prepared_default),
    ] {
        let outcome = prepared
            .apply_started(
                &session,
                submission_id.to_string(),
                TurnStartKind::Automatic,
            )
            .await
            .expect("automatic admission should return a typed rejection");
        assert!(outcome.is_none());
        assert_eq!(session.thread_settings_snapshot().await, desired_settings);
        assert!(session.active_turn.lock().await.is_none());
        assert_eq!(
            session.services.turn_environments.selections(),
            original_environments
        );
        assert!(!session.mcp_refresh.is_pending());
        assert_eq!(*records.lock().expect("config records lock"), Vec::new());
        while let Ok(event) = rx.try_recv() {
            assert_ne!(event.id, submission_id);
        }
    }

    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        Vec::<TurnInput>::new()
    );

    // The rejected candidate is valid and would have real runtime effects if
    // accepted by an ordinary settings update.
    session
        .update_settings(thread_settings::prepare_update(
            overrides,
            &session.services.selected_capability_roots,
        ))
        .await
        .expect("explicit settings update accepts the same patch");
    assert_eq!(
        session.configured_environment_selections().await,
        proposed_environments.environments
    );
    assert!(session.mcp_refresh.is_pending());
    assert_eq!(
        *records.lock().expect("config records lock"),
        vec![(AskForApproval::Never, ApprovalsReviewer::AutoReview)]
    );
}

#[tokio::test]
async fn automatic_admission_rechecks_plan_mode_during_model_lookup() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let prepared = PreparedTurnInputSettings::prepare(
        &session,
        ThreadSettingsOverrides {
            effort: Some(Some(ReasoningEffort::High)),
            ..Default::default()
        },
        TurnStartOptions::default(),
    )
    .await
    .expect("preview sparse settings");
    let mut mode = session.collaboration_mode().await;
    let model = mode.settings.model.clone();
    let (models, lookup) = GatedModelsManager::new(admission_models(&[&model]), &model);
    Arc::get_mut(&mut session)
        .expect("unique test session")
        .services
        .models_manager = models;
    let starting = prepared.apply_started(
        &session,
        "automatic-plan-during-lookup".to_string(),
        TurnStartKind::Automatic,
    );
    tokio::pin!(starting);
    tokio::select! {
        _ = &mut starting => panic!("admission must wait for model metadata"),
        () = lookup.wait_until_blocked() => {}
    }
    mode.mode = ModeKind::Plan;
    session
        .update_settings(SessionSettingsUpdate {
            step_settings: StepSettingsUpdate {
                collaboration_mode: Some(mode),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("enter Plan during admission lookup");
    let desired = session.thread_settings_snapshot().await;
    lookup.release();
    assert!(starting.await.expect("typed Plan rejection").is_none());
    assert_eq!(session.thread_settings_snapshot().await, desired);
    assert!(session.active_turn.lock().await.is_none());
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).all(|event| {
        event.id != "automatic-plan-during-lookup"
            || !matches!(event.msg, EventMsg::ThreadSettingsApplied(_))
    }));
}

// Merge-safety anchor: unsupported recipient policy must reject input before its settings or runtime effects publish.
#[tokio::test]
async fn recipient_daybreak_rejection_does_not_publish_started_input_settings() {
    const SUPPORTED: &str = "admission-supported";
    const UNSUPPORTED: &str = "admission-unsupported";
    let (mut session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let mut models = admission_models(&[SUPPORTED, UNSUPPORTED]);
    models[0].available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::DaybreakBlue],
    });
    models[1].available_access_programs = None;
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    extensions.config_contributor(Arc::new(ConfigRecorder(Arc::clone(&records))));
    let mutable = Arc::get_mut(&mut session).expect("unique test session");
    mutable.services.extensions = Arc::new(extensions.build());
    mutable.services.models_manager = Arc::new(StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse { models },
        codex_models_manager::ModelOrigin::ExplicitConfig,
    ));
    {
        let configuration = &mut mutable.state.get_mut().session_configuration;
        let config = Arc::make_mut(&mut configuration.original_config_do_not_use);
        config.agent_role_daybreak = Some(true);
        config.model_provider_id = "openai".to_string();
        let settings = Arc::make_mut(&mut configuration.step_settings);
        settings.collaboration_mode = settings.collaboration_mode.with_updates(
            Some(SUPPORTED.to_string()),
            /*effort*/ None,
            /*developer_instructions*/ None,
        );
        settings.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
        settings.approvals_reviewer = ApprovalsReviewer::User;
    }
    let current = session.state.lock().await.session_configuration.clone();
    let current_model = current
        .step_settings
        .resolve_model_info(
            session.services.models_manager.as_ref(),
            &current.model_info_overrides,
        )
        .await;
    assert_eq!(current_model.slug, SUPPORTED);
    assert_eq!(
        current_model
            .available_access_programs
            .and_then(|programs| programs.daybreak()),
        Some(CyberAccessProgram::DaybreakBlue)
    );
    let desired_settings = session.thread_settings_snapshot().await;
    let configured_environments = session.configured_environment_selections().await;
    let configured_environment_configs: Vec<_> = session
        .services
        .turn_environments
        .configured_snapshot_now()
        .refresh_readiness()
        .turn_environments()
        .map(|environment| environment.config().clone())
        .collect();
    assert!(!configured_environment_configs.is_empty());
    let active_environments = session.services.turn_environments.selections();
    let original_model_info = session.services.thread_extension_data.get::<ModelInfo>();
    let workspace = tempfile::tempdir().expect("create proposed workspace");
    let proposed_environments = local_selections(
        AbsolutePathBuf::try_from(workspace.path()).expect("absolute workspace path"),
    );
    assert_ne!(configured_environments, proposed_environments.environments);
    let _mcp_refresh = session
        .mcp_refresh
        .acquire()
        .await
        .expect("MCP refresh gate");
    session.mcp_refresh.claim();
    let result = handle(
        &session,
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "reject unsupported model".to_string(),
            text_elements: Vec::new(),
        }])
        .with_thread_settings(ThreadSettingsOverrides {
            model: Some(UNSUPPORTED.to_string()),
            environments: Some(proposed_environments.into_requests()),
            approval_policy: Some(AskForApproval::Never),
            approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
            ..Default::default()
        })
        .on_start(TurnStartOptions {
            cyber_access_program: Some(CyberAccessProgram::Standard),
            ..Default::default()
        }),
        TurnInputMode::StartIfIdle,
        "rejected-recipient".to_string(),
    )
    .await;
    let error = result.expect_err("unsupported recipient model must reject input");
    assert!(matches!(
        error.details(),
        CodexErrorDetails::InvalidRequest(message)
            if message == "Daybreak is not supported by model `admission-unsupported`."
    ));
    assert_eq!(session.thread_settings_snapshot().await, desired_settings);
    assert_eq!(
        session.configured_environment_selections().await,
        configured_environments
    );
    assert_eq!(
        session
            .services
            .turn_environments
            .configured_snapshot_now()
            .refresh_readiness()
            .turn_environments()
            .map(|environment| environment.config().clone())
            .collect::<Vec<_>>(),
        configured_environment_configs
    );
    assert_eq!(
        session.services.turn_environments.selections(),
        active_environments
    );
    assert_eq!(
        session.services.thread_extension_data.get::<ModelInfo>(),
        original_model_info
    );
    assert!(!session.mcp_refresh.is_pending());
    assert!(records.lock().expect("config records lock").is_empty());
    assert!(session.active_turn.lock().await.is_none());
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).all(|event| {
        event.id != "rejected-recipient" || !matches!(event.msg, EventMsg::ThreadSettingsApplied(_))
    }));
    assert!(
        session
            .reserve_turn_start("after-rejection")
            .await
            .is_some()
    );
    session.cancel_reserved_turn_start("after-rejection").await;
}

#[test_case(Some(true), "openai", Some(CyberAccessProgram::DaybreakBlue); "supported recipient")]
#[test_case(None, "openai", Some(CyberAccessProgram::DaybreakRed); "no override")]
#[test_case(None, "custom", None; "custom provider")]
#[tokio::test]
async fn started_input_uses_effective_recipient_program(
    policy: Option<bool>,
    provider: &str,
    expected: Option<CyberAccessProgram>,
) {
    const SUPPORTED: &str = "admission-supported";
    let (mut session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let mut models = admission_models(&[SUPPORTED]);
    models[0].available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::DaybreakBlue],
    });
    let mutable = Arc::get_mut(&mut session).expect("unique test session");
    mutable.services.models_manager = Arc::new(StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse { models },
        codex_models_manager::ModelOrigin::ExplicitConfig,
    ));
    {
        let configuration = &mut mutable.state.get_mut().session_configuration;
        let config = Arc::make_mut(&mut configuration.original_config_do_not_use);
        config.agent_role_daybreak = policy;
        config.model_provider_id = provider.to_string();
    }
    let prepared = PreparedTurnInputSettings::prepare(
        &session,
        ThreadSettingsOverrides {
            model: Some(SUPPORTED.to_string()),
            ..Default::default()
        },
        TurnStartOptions {
            cyber_access_program: Some(CyberAccessProgram::DaybreakRed),
            ..Default::default()
        },
    )
    .await
    .expect("preview supported selection");
    let turn = prepared
        .apply_started(
            &session,
            "supported-recipient".to_string(),
            TurnStartKind::User,
        )
        .await
        .expect("recipient policy accepts selection")
        .expect("user starts are unconditional");
    assert_eq!(turn.model_info().slug, SUPPORTED);
    assert_eq!(turn.cyber_access_program, expected);
    assert_eq!(
        turn.extension_data.get::<CyberAccessProgram>().as_deref(),
        expected.as_ref()
    );
}

#[test_case(TurnStartKind::User; "ordinary constructor")]
#[test_case(TurnStartKind::Automatic; "conditional constructor")]
#[tokio::test]
async fn admission_revalidates_constraints_before_committing(kind: TurnStartKind) {
    let (mut session, _turn_context, rx) = make_session_and_context_with_rx().await;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings).approval_policy =
            Constrained::allow_any(AskForApproval::OnRequest);
    }
    let prepared = PreparedTurnInputSettings::prepare(
        &session,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            service_tier: Some(Some(ServiceTier::Fast.request_value().to_string())),
            ..Default::default()
        },
        TurnStartOptions::default(),
    )
    .await
    .expect("approval-policy edit should initially be valid");
    let model = session.collaboration_mode().await.settings.model;
    let (models, lookup) = GatedModelsManager::new(admission_models(&[&model]), &model);
    Arc::get_mut(&mut session)
        .expect("unique test session")
        .services
        .models_manager = models;
    let submission_id = "constraints-after-preview";
    let starting = prepared.apply_started(&session, submission_id.to_string(), kind);
    tokio::pin!(starting);
    tokio::select! {
        _ = &mut starting => panic!("admission must wait for model metadata"),
        () = lookup.wait_until_blocked() => {}
    }

    let approval_policy = Constrained::allow_only(AskForApproval::OnRequest);
    let expected_message = CodexErr::InvalidRequest(
        approval_policy
            .can_set(&AskForApproval::Never)
            .expect_err("new constraint must reject the prepared edit")
            .to_string(),
    )
    .to_string();
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings).approval_policy =
            approval_policy;
    }
    let desired_settings = session.thread_settings_snapshot().await;
    lookup.release();
    let result = starting.await;
    let Err(error) = result else {
        panic!("commit-time constraint failure must return InvalidRequest");
    };
    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("unexpected commit-time error: {error}");
    };
    assert_eq!(message, &expected_message);
    let errors: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|event| event.id == submission_id)
        .map(|event| match event.msg {
            EventMsg::Error(error) => error,
            other => panic!("unexpected rejected-turn event: {other:?}"),
        })
        .collect();
    assert_eq!(
        errors,
        vec![ErrorEvent {
            misalignment: None,
            message: expected_message,
            codex_error_info: Some(CodexErrorInfo::BadRequest),
        }]
    );
    assert_eq!(session.thread_settings_snapshot().await, desired_settings);
    assert!(session.active_turn.lock().await.is_none());
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        Vec::<TurnInput>::new()
    );
}

#[tokio::test]
async fn start_only_accepts_user_input_in_plan_mode() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let mut collaboration_mode = session.collaboration_mode().await;
    collaboration_mode.mode = ModeKind::Plan;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings).collaboration_mode =
            collaboration_mode;
        state.merge_connector_selection(["calendar".to_string()]);
    }

    let submission = submit_start_only(
        &session,
        SubmittedTurnInput::UserInput {
            content: vec![UserInput::Text {
                text: "queued user input".to_string(),
                text_elements: Vec::new(),
            }],
            client_id: Some("queued-user-message".to_string()),
        },
    )
    .await;
    assert!(matches!(submission, TurnInputSubmission::Started { .. }));
    assert!(
        session
            .state
            .lock()
            .await
            .get_connector_selection()
            .is_empty()
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn start_only_rejects_empty_user_input_in_plan_mode() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let mut collaboration_mode = session.collaboration_mode().await;
    collaboration_mode.mode = ModeKind::Plan;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings).collaboration_mode =
            collaboration_mode;
    }

    let submission = submit_start_only(
        &session,
        SubmittedTurnInput::UserInput {
            content: Vec::new(),
            client_id: Some("empty-queued-user-message".to_string()),
        },
    )
    .await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        },
        submission
    );
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn start_only_rejects_pending_trigger_turn_without_injecting() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings)
            .collaboration_mode
            .mode = ModeKind::Plan;
    }
    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                AgentPath::root(),
                AgentPath::root(),
                Vec::new(),
                "pending trigger".to_string(),
                /*trigger_turn*/ true,
            )
            .into(),
            Default::default(),
        )
        .await;

    let submission = submit_start_only(
        &session,
        SubmittedTurnInput::ResponseItem(user_message("synthetic idle input")),
    )
    .await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        },
        submission
    );
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.input_queue.has_trigger_turn_mailbox_items().await);
    assert_eq!(session.collaboration_mode().await.mode, ModeKind::Plan);
}

/// Terminal errors bypass only the ordinary message-wake flag, not admission guards.
#[test_case(SessionSource::Exec, true, ModeKind::Default, true, false, false, false; "interrupted_root")]
#[test_case(SessionSource::Exec, false, ModeKind::Plan, true, false, false, false; "plan_root")]
#[test_case(SessionSource::SubAgent(SubAgentSource::Review), false, ModeKind::Default, true, false, false, false; "inherited_child_flag")]
#[test_case(SessionSource::Exec, false, ModeKind::Default, false, true, false, true; "terminal_error_idle_root")]
#[test_case(SessionSource::Exec, false, ModeKind::Default, false, false, false, false; "ordinary_completion_wake_disabled")]
#[test_case(SessionSource::Exec, true, ModeKind::Default, false, true, false, false; "terminal_error_interrupted_root")]
#[test_case(SessionSource::Exec, false, ModeKind::Plan, false, true, false, false; "terminal_error_plan_root")]
#[test_case(SessionSource::SubAgent(SubAgentSource::Review), false, ModeKind::Default, false, true, false, false; "terminal_error_child")]
#[test_case(SessionSource::Exec, false, ModeKind::Default, false, true, true, false; "terminal_error_active_parent")]
#[tokio::test]
async fn agent_message_wake_reservation_preserves_existing_boundaries(
    source: SessionSource,
    interrupted: bool,
    mode: ModeKind,
    wake_on_messages: bool,
    terminal_error_wake: bool,
    active: bool,
    expected_wake: bool,
) {
    let (mut session, turn_context) = make_session_and_context().await;
    session
        .features
        .set_enabled(Feature::WakeOnAgentMessages, wake_on_messages)
        .expect("set agent-message wakes");
    let attribution = codex_history::TurnAttribution {
        turn_id: "previous-turn".to_string(),
        turn_trigger: Some("previous-trigger".to_string()),
        parent_turn_id: None,
        initiating_agent_path: None,
        root_turn_id: Some("root-turn".to_string()),
    };
    {
        let mut state = session.state.lock().await;
        state.session_configuration.session_source = source;
        state.turn_attribution = Some(attribution.clone());
        Arc::make_mut(&mut state.session_configuration.step_settings)
            .collaboration_mode
            .mode = mode;
    }
    if interrupted {
        session.mark_interrupted();
    }
    if active {
        *session.active_turn.lock().await = Some(ActiveTurn {
            reserved_turn_id: Some("existing-turn".to_string()),
            ..Default::default()
        });
    }
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("valid worker path"),
        AgentPath::root(),
        Vec::new(),
        "late worker update".to_string(),
        /*trigger_turn*/ false,
    );
    session
        .input_queue
        .enqueue_mailbox_communication(
            crate::PendingAgentMessage::new(communication.clone(), None, terminal_error_wake),
            Default::default(),
        )
        .await;

    let reservation = session
        .reserve_pending_work_turn(&turn_context.sub_id)
        .await;
    assert_eq!(reservation.is_some(), expected_wake);
    if let Some((_, options)) = reservation {
        assert_eq!(options.turn_trigger, attribution.turn_trigger);
        assert_eq!(options.root_turn_id, attribution.root_turn_id);
        assert_eq!(options.parent_turn_id, attribution.parent_turn_id);
        assert_eq!(
            options.initiating_agent_path,
            attribution.initiating_agent_path
        );
    }
    let active_turn = session.active_turn.lock().await;
    if active {
        assert_eq!(
            active_turn.as_ref().unwrap().reserved_turn_id.as_deref(),
            Some("existing-turn")
        );
    } else {
        assert_eq!(active_turn.is_some(), expected_wake);
    }
    drop(active_turn);
    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);
    assert_eq!(
        session.input_queue.drain_mailbox_input_items().await.0,
        vec![TurnInput::InterAgentCommunication(communication.into())]
    );
    assert!(!session.input_queue.has_terminal_error_mailbox_items().await);
}

#[test_case(true; "plan_after_reservation")]
#[test_case(false; "interrupt_after_reservation")]
#[tokio::test]
async fn reserved_terminal_error_wake_revalidates_before_consumption(enter_plan: bool) {
    let (mut session, _turn_context) = make_session_and_context().await;
    session
        .features
        .set_enabled(Feature::WakeOnAgentMessages, false)
        .expect("disable ordinary message wakes");
    let session = Arc::new(session);
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("valid worker path"),
        AgentPath::root(),
        Vec::new(),
        "terminal error".to_string(),
        /*trigger_turn*/ false,
    );
    session
        .input_queue
        .enqueue_mailbox_communication(
            crate::PendingAgentMessage::new(communication.clone(), None, true),
            Default::default(),
        )
        .await;
    let (turn_state, _) = session
        .reserve_pending_work_turn("reserved-error-wake")
        .await
        .expect("terminal error should reserve an idle root");

    if enter_plan {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings)
            .collaboration_mode
            .mode = ModeKind::Plan;
    } else {
        session.abort_all_tasks(TurnAbortReason::Interrupted).await;
        assert!(session.is_interrupted());
    }
    assert!(
        session
            .pending_work_turn_configuration(&turn_state)
            .await
            .is_none()
    );
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.input_queue.has_terminal_error_mailbox_items().await);
    session.maybe_start_turn_for_pending_work().await;
    assert!(session.active_turn.lock().await.is_none());

    let follow_up = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("valid worker path"),
        AgentPath::root(),
        Vec::new(),
        "explicit follow-up".to_string(),
        /*trigger_turn*/ true,
    );
    session
        .input_queue
        .enqueue_mailbox_communication(follow_up.clone().into(), Default::default())
        .await;
    let (turn_state, _) = session
        .reserve_pending_work_turn("explicit-follow-up")
        .await
        .expect("explicit follow-up must remain admissible");
    assert!(
        session
            .pending_work_turn_configuration(&turn_state)
            .await
            .is_some()
    );
    session
        .cancel_reserved_turn_start("explicit-follow-up")
        .await;
    assert_eq!(
        session.input_queue.drain_mailbox_input_items().await.0,
        vec![
            TurnInput::InterAgentCommunication(communication.into()),
            TurnInput::InterAgentCommunication(follow_up.into()),
        ]
    );
}

#[tokio::test]
async fn interrupted_error_wake_keeps_automatic_origin_after_drain() {
    let (mut session, _turn_context) = make_session_and_context().await;
    session
        .features
        .set_enabled(Feature::WakeOnAgentMessages, false)
        .expect("disable ordinary message wakes");
    let session = Arc::new(session);
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("valid worker path"),
        AgentPath::root(),
        Vec::new(),
        "terminal error".to_string(),
        /*trigger_turn*/ false,
    );
    session
        .input_queue
        .enqueue_mailbox_communication(
            crate::PendingAgentMessage::new(communication.clone(), None, true),
            Default::default(),
        )
        .await;
    let (turn_state, _) = session
        .reserve_pending_work_turn("reserved-error-wake")
        .await
        .expect("terminal error should reserve an idle root");
    assert!(
        session
            .pending_work_turn_configuration(&turn_state)
            .await
            .is_some()
    );
    let (input, _) = session
        .input_queue
        .get_pending_input(&session.active_turn)
        .await;
    assert!(
        matches!(input.as_slice(), [TurnInput::InterAgentCommunication(mail)] if mail.terminal_error_wake)
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
    assert!(session.is_interrupted());
    assert!(
        session
            .pending_work_turn_configuration(&turn_state)
            .await
            .is_none()
    );
    assert!(session.active_turn.lock().await.is_none());
    session
        .input_queue
        .enqueue_mailbox_communication(
            crate::PendingAgentMessage::new(communication, None, true),
            Default::default(),
        )
        .await;
    assert!(
        session
            .reserve_pending_work_turn("later-error-wake")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn steer_only_requires_active_turn() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let submission = submit_steer_only(
        &session,
        vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }],
        "missing-turn-id",
    )
    .await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NoActiveTurn,
        },
        submission
    );
}

#[tokio::test]
async fn steer_only_enforces_expected_turn_id() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    turn_context
        .turn_metadata_state
        .set_turn_trigger("composer".to_string());
    session
        .spawn_task(
            Arc::clone(&turn_context),
            vec![TurnInput::UserInput {
                metadata: Default::default(),
                content: vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                client_id: None,
            }],
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: false,
            },
        )
        .await;

    let submission = submit_steer_only(
        &session,
        vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }],
        "different-turn-id",
    )
    .await;
    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ExpectedTurnMismatch {
                expected: "different-turn-id".to_string(),
                actual: turn_context.sub_id.clone(),
            },
        },
        submission
    );

    let output: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "function_call_output",
        "name": "send_message_to_thread",
        "output": "delegated work",
    }))
    .expect("valid standalone output");

    let submission = handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::ResponseItem(output)).on_start(
            TurnStartOptions {
                turn_trigger: Some("automation_cron_scheduled".to_string()),
                ..Default::default()
            },
        ),
        TurnInputMode::StartOrSteer,
        "test-submission".to_string(),
    )
    .await
    .expect("standalone output should steer the active turn");

    assert_eq!(
        submission,
        TurnInputSubmission::Steered {
            root_turn_id: turn_context.sub_id.clone(),
            turn_id: turn_context.sub_id.clone()
        }
    );
    assert_eq!(
        turn_context
            .turn_metadata_state
            .current_turn_trigger()
            .as_deref(),
        Some("composer")
    );
    let turn_state = session
        .input_queue
        .turn_state_for_sub_id(&session.active_turn, &turn_context.sub_id)
        .await
        .expect("active turn state");
    assert_eq!(
        session
            .input_queue
            .subscribe_activity(Some(turn_state.as_ref()))
            .await
            .1,
        Some(crate::session::input_queue::InputQueueActivity::Steer)
    );
}

#[tokio::test]
async fn rejects_non_regular_turns() {
    for (task_kind, turn_kind) in [
        (TaskKind::Review, NonSteerableTurnKind::Review),
        (TaskKind::Compact, NonSteerableTurnKind::Compact),
    ] {
        let (session, incoming_turn_context, _rx) = make_session_and_context_with_rx().await;
        incoming_turn_context
            .turn_metadata_state
            .set_root_turn_id("incoming-root".to_string());
        let turn_context = session
            .new_turn_with_default_settings("turn".to_string(), Default::default())
            .await
            .expect("valid work context");
        turn_context
            .turn_metadata_state
            .set_root_turn_id("active-root".to_string());
        session
            .spawn_task(
                Arc::clone(&turn_context),
                vec![TurnInput::UserInput {
                    metadata: Default::default(),
                    content: vec![UserInput::Text {
                        text: "hello".to_string(),
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
                NeverEndingTask {
                    kind: task_kind,
                    listen_to_cancellation_token: true,
                },
            )
            .await;

        let steer_input = vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }];
        let steer_submission = submit_steer_only(&session, steer_input.clone(), "turn").await;
        assert_eq!(
            TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::ActiveTurnNotSteerable { turn_kind },
            },
            steer_submission
        );
        let start_or_steer_submission = handle(
            &session,
            TurnInputRequest::user_input(steer_input),
            TurnInputMode::StartOrSteer,
            "test-submission".to_string(),
        )
        .await
        .expect("start-or-steer submission should be valid");
        assert_eq!(
            TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::ActiveTurnNotSteerable { turn_kind },
            },
            start_or_steer_submission
        );
        assert_eq!(
            turn_context.turn_metadata_state.root_turn_id().as_deref(),
            Some("active-root")
        );

        session.abort_all_tasks(TurnAbortReason::Interrupted).await;
    }
}

#[test_case("automation_heartbeat_scheduled", None, UserInputOrigin::User; "human")]
#[test_case("composer", Some("automation_heartbeat_scheduled"), UserInputOrigin::Heartbeat; "scheduled")]
#[tokio::test]
async fn steer_preserves_request_origin(
    active_trigger: &str,
    request_trigger: Option<&str>,
    origin: UserInputOrigin,
) {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    turn_context
        .turn_metadata_state
        .set_turn_trigger(active_trigger.to_owned());
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: false,
            },
        )
        .await;
    let content = vec![UserInput::Text {
        text: "Create the worktree now.".to_owned(),
        text_elements: Vec::new(),
    }];
    // Merge-safety anchor: steering retains client metadata and request provenance while its order waits for confirmed delivery, without retaining ActiveTurn.
    let checkpoint = thread_settings::acquire_persistence_lock(&session).await;
    let (recording, _) =
        session.record_delivered_assistant_message(codex_history::RetainedUserMessage {
            origin: UserInputOrigin::User,
            turn_id: turn_context.sub_id.clone(),
            message_id: Some("confirmed-send".to_owned()),
            text: "May I publish?".to_owned(),
            complete: true,
            phase: None,
        });
    let mut submission = Box::pin(handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::UserInput {
            content: content.clone(),
            client_id: Some("steered-client-message".to_owned()),
        })
        .on_start(TurnStartOptions {
            turn_trigger: request_trigger.map(str::to_owned),
            ..Default::default()
        })
        .with_responses_metadata(Some(HashMap::from([(
            "workspace_kind".to_owned(),
            "steered-workspace".to_owned(),
        )]))),
        TurnInputMode::Steer {
            expected_turn_id: turn_context.sub_id.clone(),
        },
        "steer-submission".to_owned(),
    ));
    assert!(futures::poll!(submission.as_mut()).is_pending());
    assert!(
        session.active_turn.try_lock().is_ok(),
        "ordering waits must leave ActiveTurn available"
    );
    drop(checkpoint);
    assert_eq!(
        submission.await.unwrap(),
        TurnInputSubmission::Steered {
            root_turn_id: turn_context.root_turn_id(),
            turn_id: turn_context.sub_id.clone()
        }
    );
    recording
        .await
        .expect("confirmed delivery finishes before steering is accepted");
    assert_eq!(
        turn_context.turn_metadata_state.workspace_kind().as_deref(),
        Some("steered-workspace")
    );
    let pending = session
        .input_queue
        .get_pending_input(&session.active_turn)
        .await
        .0;
    assert_eq!(
        pending,
        vec![TurnInput::UserInput {
            content,
            client_id: Some("steered-client-message".to_owned()),
            metadata: crate::session::UserInputMetadata {
                acceptance_order: Some(1),
                origin,
                ..Default::default()
            },
        }]
    );
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn incompatible_recipient_policy_cancels_idle_admission_reservation() {
    let (session, _turn_context, _events) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            codex_login::CodexAuth::from_api_key("test-api-key"),
            Vec::new(),
            |config| {
                config.agent_role_daybreak = Some(true);
                config.model_provider_id = "custom".to_string();
            },
        )
        .await;
    let error = handle(
        &session,
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "reject before sampling".to_string(),
            text_elements: Vec::new(),
        }]),
        TurnInputMode::StartIfIdle,
        "rejected-role-policy".to_string(),
    )
    .await
    .expect_err("unsupported policy must reject admission");
    assert!(error.to_string().contains("Daybreak"));
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.reserve_turn_start("next-admission").await.is_some());
    session.cancel_reserved_turn_start("next-admission").await;
}

#[tokio::test]
async fn scored_prompt_keeps_its_submission_item_id() {
    let (session, turn, events) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&turn),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: false,
            },
        )
        .await;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    // A legacy steer can present the same UI client ID as an admitted steer.
    for id in [Some(first), None, Some(second)] {
        let mut request = TurnInputRequest::new(SubmittedTurnInput::UserInput {
            content: vec![UserInput::Text {
                text: "identical input".into(),
                text_elements: vec![],
            }],
            client_id: Some("same-client-id".into()),
        });
        request.user_item_uuid = id;
        handle(
            &session,
            request,
            TurnInputMode::Steer {
                expected_turn_id: turn.sub_id.clone(),
            },
            Uuid::now_v7().to_string(),
        )
        .await
        .unwrap();
    }
    let (pending, _) = session
        .input_queue
        .get_pending_input(&session.active_turn)
        .await;
    for input in pending {
        let TurnInput::UserInput {
            content,
            client_id,
            metadata,
        } = input
        else {
            panic!("expected admitted user input");
        };
        session
            .record_user_prompt_and_emit_turn_item(
                &turn,
                turn.model_info(),
                &content,
                client_id,
                metadata,
                codex_thread_store::PersistContext::Standard,
            )
            .await;
    }
    let history = session.clone_history().await;
    let ids: Vec<_> = history
        .raw_items()
        .filter_map(|item| {
            if let ResponseItem::Message { role, id, .. } = item
                && role == "user"
            {
                id.as_ref().map(ToString::to_string)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids[0], format!("msg_input_{first}"));
    assert!(!ids[1].starts_with("msg_input_"));
    assert_eq!(ids[2], format!("msg_input_{second}"));
    let ui_ids = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.msg {
            codex_protocol::protocol::EventMsg::ItemCompleted(event) => {
                let item = codex_app_server_protocol::ThreadItemPayload::from(event.item);
                if let codex_app_server_protocol::ThreadItemPayload::UserMessage {
                    id,
                    client_id,
                    ..
                } = item
                {
                    assert_eq!(client_id.as_deref(), Some("same-client-id"));
                    return Some(id);
                }
                None
            }
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ui_ids.len(), 3);
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}
