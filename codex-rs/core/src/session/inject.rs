use super::input_queue::TurnInput;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::codex_thread::TryStartTurnIfIdleError;
use crate::codex_thread::TryStartTurnIfIdleRejectionReason;
use codex_analytics::ImagePreparationMetadata;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use std::sync::Arc;

impl Session {
    /// Returns the input if there is no active turn to inject into.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_if_running<T: Into<ResponseItemEnvelope>>(
        &self,
        input: Vec<T>,
    ) -> Result<(), Vec<T>> {
        let active_turn = self.active_turn.lock().await;
        let Some(active_turn) = active_turn.as_ref().filter(|turn| turn.task.is_some()) else {
            return Err(input);
        };
        let turn_state = Arc::clone(&active_turn.turn_state);
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                turn_state.as_ref(),
                input
                    .into_iter()
                    .map(Into::into)
                    .map(TurnInput::ResponseItem)
                    .collect(),
            )
            .await;
        Ok(())
    }

    /// Merge-safety anchor: retain ActiveTurn-aware injection and delivery without source-turn ambiguity.
    /// Injects hook context into the running turn atomically.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn provenance and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_hook_context_if_running(
        &self,
        input: Vec<ResponseItem>,
    ) -> Result<(), Vec<ResponseItem>> {
        let active_turn = self.active_turn.lock().await;
        let Some(active_turn) = active_turn.as_ref().filter(|turn| turn.task.is_some()) else {
            return Err(input);
        };
        let turn_state = Arc::clone(&active_turn.turn_state);
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

    /// Merge-safety anchor: trusted client provenance stays attached through transition waits,
    /// queued delivery, and history fallback.
    /// Preserves trusted client provenance while items wait for an active turn.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active-turn admission and history fallback must remain atomic"
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
            let active_turn = self.active_turn.lock().await;
            let Some(turn) = active_turn.as_ref() else {
                self.record_annotated_conversation_items(
                    turn_context,
                    turn_context.model_info(),
                    items,
                )
                .await;
                return;
            };
            let Some(task) = turn.task.as_ref() else {
                self.record_annotated_conversation_items(
                    turn_context,
                    turn_context.model_info(),
                    items,
                )
                .await;
                return;
            };
            if turn.finishing {
                let mut done = Box::pin(Arc::clone(&task.done).notified_owned());
                let _ = done.as_mut().enable();
                drop(active_turn);
                done.await;
                continue;
            }
            let turn_state = Arc::clone(&turn.turn_state);
            self.input_queue
                .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                    turn_state.as_ref(),
                    items.into_iter().map(TurnInput::ResponseItem).collect(),
                )
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
        model_info: &ModelInfo,
        items: Vec<ResponseItemEnvelope>,
    ) {
        if items.iter().all(|item| item.metadata.is_none()) {
            let items = items
                .into_iter()
                .map(ResponseItemEnvelope::into_item)
                .collect::<Vec<_>>();
            self.record_conversation_items(turn_context, model_info, &items)
                .await;
            return;
        }

        let (annotated_items, image_preparations) = self
            .prepare_annotated_conversation_items_for_history(turn_context, model_info, items)
            .await;
        self.record_prepared_conversation_items(
            turn_context,
            model_info,
            annotated_items,
            image_preparations,
        )
        .await;
    }

    pub(crate) async fn prepare_annotated_conversation_items_for_history(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        items: Vec<ResponseItemEnvelope>,
    ) -> (Vec<ResponseItemEnvelope>, Vec<ImagePreparationMetadata>) {
        let mut annotated_items = Vec::with_capacity(items.len());
        let mut image_preparations = Vec::new();
        for envelope in items {
            let (prepared_items, prepared_images) = self
                .prepare_conversation_items_for_history(
                    turn_context,
                    model_info,
                    std::slice::from_ref(&envelope.item),
                )
                .await;
            image_preparations.extend(prepared_images);

            let mut metadata = envelope.metadata;
            annotated_items.extend(prepared_items.into_owned().into_iter().map(|item| {
                ResponseItemEnvelope {
                    item,
                    metadata: metadata.take(),
                }
            }));
        }
        (annotated_items, image_preparations)
    }

    // Merge-safety anchor: automatic idle admission rechecks trigger-turn mail, rejects Plan-mode
    // auto-start, returns original input, and cancels its unopened ActiveTurn reservation.
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
        let Some(turn_state) = self.reserve_turn_start(&sub_id).await else {
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::Busy,
                input,
            ));
        };
        self.try_start_reserved_idle_turn(turn_state, sub_id, input)
            .await
    }

    async fn try_start_reserved_idle_turn(
        self: &Arc<Self>,
        turn_state: Arc<tokio::sync::Mutex<crate::state::TurnState>>,
        sub_id: String,
        input: Vec<TurnInput>,
    ) -> Result<(), TryStartTurnIfIdleError> {
        let has_user_input = input.iter().any(
            |item| matches!(item, TurnInput::UserInput { content, .. } if !content.is_empty()),
        );
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.cancel_reserved_turn_start(&sub_id).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }

        let turn_context = self
            .new_turn_with_default_settings(sub_id.clone(), Default::default())
            .await;
        if !has_user_input && turn_context.mode() == ModeKind::Plan {
            self.cancel_reserved_turn_start(&sub_id).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PlanMode,
                input,
            ));
        }
        self.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
            .await;
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.cancel_reserved_turn_start(&sub_id).await;
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
                .extend_pending_input_for_turn_state(turn_state.as_ref(), input)
                .await;
            Vec::new()
        };

        if !self
            .start_task(turn_context, task_input, crate::tasks::RegularTask::new())
            .await
        {
            self.cancel_reserved_turn_start(&sub_id).await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::Busy,
                original_input,
            ));
        }
        Ok(())
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
        self.record_conversation_items(turn_context, turn_context.model_info(), &items)
            .await;
    }
}

#[cfg(test)]
#[path = "inject_tests.rs"]
mod tests;
