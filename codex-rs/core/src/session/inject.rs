use super::input_queue::TurnInput;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::codex_thread::TryStartTurnIfIdleError;
use crate::codex_thread::TryStartTurnIfIdleRejectionReason;
use crate::state::TurnStartClaim;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ResponseItem;
use std::sync::Arc;
use tracing::Instrument;

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
                input.into_iter().map(TurnInput::ResponseItem).collect(),
            )
            .await;
        Ok(())
    }

    /// Starts a regular turn with the provided items only if automatic idle work
    /// is allowed for the current session state.
    ///
    /// This is the shared gate for extension-initiated idle work. It refuses to
    /// start a turn when user/client-triggered work is queued, any task is still
    /// active, or the session is currently in Plan mode. Active Review tasks are
    /// covered by the active-task check because Review turns are not steerable.
    pub(crate) async fn try_start_turn_if_idle(
        self: &Arc<Self>,
        input: Vec<ResponseItem>,
    ) -> Result<(), TryStartTurnIfIdleError> {
        if input.is_empty() {
            return Ok(());
        }
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }
        if self.collaboration_mode().await.mode == ModeKind::Plan {
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
            .instrument(tracing::Span::current()),
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
        input: Vec<ResponseItem>,
    ) -> Result<(), TryStartTurnIfIdleError> {
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.cancel_claimed_start(&claim).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }

        let turn_context = self.new_default_turn_with_sub_id(sub_id).await;
        if turn_context.mode == ModeKind::Plan {
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
        let failed_start_input = input.clone();
        self.input_queue
            .extend_pending_input_for_turn_state(
                claim.turn_state.as_ref(),
                input.into_iter().map(TurnInput::ResponseItem).collect(),
            )
            .await;
        match self
            .start_claimed_regular_task(claim, turn_context, Vec::new())
            .await
        {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!(%err, "failed to install claimed idle turn");
                Err(TryStartTurnIfIdleError::new(
                    TryStartTurnIfIdleRejectionReason::Busy,
                    failed_start_input,
                ))
            }
        }
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
