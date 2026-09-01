use super::input_queue::TurnInput;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::codex_thread::TryStartTurnIfIdleError;
use crate::codex_thread::TryStartTurnIfIdleRejectionReason;
use crate::state::TurnStartClaim;
use crate::tasks::MailboxParentProvenance;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ResponseItem;
use std::sync::Arc;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

impl Session {
    /// Returns the input if there is no active turn to inject into.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn inject_if_running(
        &self,
        input: Vec<ResponseItem>,
    ) -> Result<(), Vec<ResponseItem>> {
        let slot = self.active_turn.lock().await;
        if slot.running_task().is_none() {
            return Err(input);
        }
        let Some(turn_state) = slot.turn_state() else {
            return Err(input);
        };
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                turn_state.as_ref(),
                input
                    .into_iter()
                    .map(ResponseItemEnvelope::new)
                    .map(TurnInput::ResponseItem)
                    .collect(),
            )
            .await;
        Ok(())
    }

    /// Injects hook context while classifying its actual receiving turn atomically.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn provenance and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_hook_context_if_running(
        &self,
        input: Vec<ResponseItem>,
        source_turn_id: Option<&str>,
    ) -> Result<(), Vec<ResponseItem>> {
        let slot = self.active_turn.lock().await;
        let Some(task) = slot.running_task() else {
            return Err(input);
        };
        let Some(turn_state) = slot.turn_state() else {
            return Err(input);
        };
        if source_turn_id != Some(task.turn_context.sub_id.as_str()) {
            task.turn_context
                .turn_metadata_state
                .mark_root_turn_ambiguous();
        }
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                turn_state.as_ref(),
                input
                    .into_iter()
                    .map(ResponseItemEnvelope::new)
                    .map(TurnInput::ResponseItem)
                    .collect(),
            )
            .await;
        Ok(())
    }

    /// Preserves trusted client provenance while items wait for an active turn.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_client_response_items(
        &self,
        items: Vec<ResponseItem>,
        turn_context: &TurnContext,
    ) {
        let items = items
            .into_iter()
            .map(|item| self.annotate_client_response_item(item))
            .collect::<Vec<_>>();
        loop {
            let slot = self.active_turn.lock().await;
            if slot.is_transitioning() {
                let mut generation_rx = slot.subscribe_generation();
                drop(slot);
                generation_rx
                    .changed()
                    .await
                    .expect("turn-slot generation sender remains live while the session is active");
                continue;
            }
            if let Some(turn_state) = slot.turn_state() {
                if let Some(task) = slot.running_task() {
                    task.turn_context
                        .turn_metadata_state
                        .mark_root_turn_ambiguous();
                }
                self.input_queue
                    .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                        turn_state.as_ref(),
                        items.into_iter().map(TurnInput::ResponseItem).collect(),
                    )
                    .await;
                return;
            }
            drop(slot);
            self.record_annotated_conversation_items(turn_context, items)
                .await;
            return;
        }
    }

    pub(crate) fn annotate_client_response_item(&self, item: ResponseItem) -> ResponseItemEnvelope {
        let metadata = (self.enabled(Feature::RetainClientDeveloperMessages)
            && matches!(&item, ResponseItem::Message { role, .. } if role == "developer"))
        .then_some(CodexHarnessMetadata {
            client_authored: true,
            ..Default::default()
        });

        ResponseItemEnvelope { item, metadata }
    }

    pub(crate) async fn record_annotated_conversation_items(
        &self,
        turn_context: &TurnContext,
        items: Vec<ResponseItemEnvelope>,
    ) {
        if items.iter().all(|item| item.metadata.is_none()) {
            let items = items
                .into_iter()
                .map(ResponseItemEnvelope::into_item)
                .collect::<Vec<_>>();
            self.record_conversation_items(turn_context, &items).await;
            return;
        }

        let mut annotated_items = Vec::with_capacity(items.len());
        let mut image_preparations = Vec::new();
        for envelope in items {
            let (prepared_items, prepared_images) = self.prepare_conversation_items_for_history(
                turn_context,
                std::slice::from_ref(&envelope.item),
            );
            image_preparations.extend(prepared_images);

            let mut metadata = envelope.metadata;
            annotated_items.extend(prepared_items.into_owned().into_iter().map(|item| {
                ResponseItemEnvelope {
                    item,
                    metadata: metadata.take(),
                }
            }));
        }
        self.record_prepared_conversation_items(turn_context, annotated_items, image_preparations)
            .await;
    }

    /// Starts a regular turn with the provided input only if automatic idle work
    /// is allowed for the current session state.
    ///
    /// This is the shared gate for extension-initiated idle work. It refuses to
    /// start a turn when user/client-triggered work is queued or any task is
    /// still active. Work without user input is also rejected in Plan mode.
    /// Active Review tasks are covered by the active-task check because Review
    /// turns are not steerable.
    pub(crate) async fn try_start_turn_if_idle(
        self: &Arc<Self>,
        input: Vec<TurnInput>,
    ) -> Result<(), TryStartTurnIfIdleError> {
        if input.is_empty() {
            return Ok(());
        }
        let has_user_input = input.iter().any(
            |item| matches!(item, TurnInput::UserInput { content, .. } if !content.is_empty()),
        );
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }
        if !has_user_input && self.collaboration_mode().await.mode == ModeKind::Plan {
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PlanMode,
                input,
            ));
        }

        let sub_id = uuid::Uuid::new_v4().to_string();
        let claim = {
            let mut slot = self.active_turn.lock().await;
            if !slot.is_idle() {
                return Err(TryStartTurnIfIdleError::new(
                    TryStartTurnIfIdleRejectionReason::Busy,
                    input,
                ));
            }
            slot.claim_start(sub_id.clone()).map_err(|_| {
                TryStartTurnIfIdleError::new(TryStartTurnIfIdleRejectionReason::Busy, input.clone())
            })?
        };

        let failed_start_input = input.clone();
        let session = Arc::clone(self);
        let startup = tokio::spawn(
            async move {
                session
                    .try_start_claimed_idle_turn(claim, sub_id, input)
                    .await
            }
            .in_current_span()
            .with_current_subscriber(),
        );
        match startup.await {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!(%err, "idle turn startup task failed");
                Err(TryStartTurnIfIdleError::new(
                    TryStartTurnIfIdleRejectionReason::Busy,
                    failed_start_input,
                ))
            }
        }
    }

    async fn try_start_claimed_idle_turn(
        self: &Arc<Self>,
        claim: TurnStartClaim,
        sub_id: String,
        input: Vec<TurnInput>,
    ) -> Result<(), TryStartTurnIfIdleError> {
        let has_user_input = input.iter().any(
            |item| matches!(item, TurnInput::UserInput { content, .. } if !content.is_empty()),
        );
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.cancel_claimed_start(&claim).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }

        let turn_context = self
            .new_turn_with_default_settings(sub_id, Default::default())
            .await;
        if !has_user_input && turn_context.mode() == ModeKind::Plan {
            self.cancel_claimed_start(&claim).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PlanMode,
                input,
            ));
        }
        self.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
            .await;
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.cancel_claimed_start(&claim).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }

        let original_input = input.clone();
        let task_input = if has_user_input {
            self.clear_connector_selection().await;
            for item in &input {
                if let TurnInput::UserInput { content, .. } = item {
                    turn_context.session_telemetry.user_prompt(content);
                }
            }
            input
        } else {
            self.input_queue
                .extend_pending_input_for_turn_state(claim.turn_state.as_ref(), input)
                .await;
            Vec::new()
        };

        let start_result = self
            .start_claimed_regular_task_with_options(
                claim,
                turn_context,
                task_input,
                None,
                MailboxParentProvenance::Ignore,
            )
            .await;
        if let Err(err) = start_result {
            tracing::warn!(%err, "failed to install claimed idle turn");
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::Busy,
                original_input,
            ));
        }
        Ok(())
    }

    pub(crate) async fn cancel_claimed_start(&self, claim: &TurnStartClaim) {
        let mut slot = self.active_turn.lock().await;
        if let Err(err) = slot.cancel_start(claim) {
            tracing::warn!(%err, "failed to cancel claimed turn startup");
        }
    }

    /// Injects items into active work, or records them without starting a turn.
    pub(crate) async fn inject_no_new_turn(
        &self,
        items: Vec<ResponseItem>,
        current_turn_context: Option<&TurnContext>,
    ) {
        let Err(items) = self.inject_if_running(items).await else {
            return;
        };
        let default_turn_context;
        let turn_context = match current_turn_context {
            Some(turn_context) => turn_context,
            None => {
                default_turn_context = self.new_default_turn().await;
                default_turn_context.as_ref()
            }
        };
        self.record_conversation_items(turn_context, &items).await;
    }
}
