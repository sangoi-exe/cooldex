use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use tracing::warn;

use super::Session;
use super::TurnContext;
use super::recall::RecallContextError;
use super::recall::RecallLoadError;
use crate::context::ContextualUserFragment;
use crate::context::PostCompactRecallContext;
use crate::context::PostCompactRecoveryContext;
use crate::context::PostCompactRecoveryContextError;
use crate::state::PostCompactRecoveryFailureClass;
use crate::state::PostCompactRecoveryIdentity;

#[derive(Debug)]
pub(super) struct PreparedPostCompactRecovery {
    identity: PostCompactRecoveryIdentity,
    insertion_index: usize,
    item_count: usize,
}

impl PreparedPostCompactRecovery {
    pub(super) fn identity(&self) -> &PostCompactRecoveryIdentity {
        &self.identity
    }

    pub(super) fn remove_from_input(self, input: &mut Vec<ResponseItem>) -> CodexResult<()> {
        let has_recovery_item = input.get(self.insertion_index).is_some_and(|item| {
            matches!(
                item,
                ResponseItem::Message { role, content, .. }
                    if role == "developer"
                        && content.iter().any(|content| {
                            matches!(
                                content,
                                codex_protocol::models::ContentItem::InputText { text }
                                    if PostCompactRecoveryContext::matches_text(text)
                            )
                        })
            )
        });
        let has_recall_item = self.item_count == 1
            || input.get(self.insertion_index + 1).is_some_and(|item| {
                matches!(
                    item,
                    ResponseItem::Message { role, content, .. }
                        if role == "user"
                            && content.iter().any(|content| {
                                matches!(
                                    content,
                                    codex_protocol::models::ContentItem::InputText { text }
                                        if PostCompactRecallContext::matches_text(text)
                                )
                            })
                )
            });
        if !has_recovery_item || !has_recall_item {
            return Err(CodexErr::Fatal(
                "post-compact recovery prompt carrier lost a transient item".to_string(),
            ));
        }
        input.drain(self.insertion_index..self.insertion_index + self.item_count);
        Ok(())
    }
}

impl Session {
    pub(super) async fn prepare_post_compact_recovery(
        &self,
        turn_context: &TurnContext,
        input: &mut Vec<ResponseItem>,
    ) -> CodexResult<Option<PreparedPostCompactRecovery>> {
        let (identity, cached_packet) = {
            let state = self.state.lock().await;
            let Some(identity) = state.post_compact_recovery.pending_identity().cloned() else {
                return match state.post_compact_recovery.blocked_failure() {
                    Some(failure) => Err(blocked_error(failure)),
                    None => Ok(None),
                };
            };
            let cached_packet = state
                .post_compact_recovery
                .packet(&identity)
                .map_err(blocked_error)?;
            (identity, cached_packet)
        };

        let packet = match cached_packet {
            Some(packet) => packet,
            None => {
                let recall = match self.load_current_thread_recall_context(turn_context).await {
                    Ok(recall) => Some(recall),
                    Err(RecallLoadError::Source(error)) => {
                        warn!(
                            %error,
                            "post-compact recall source unavailable; injecting boundary only"
                        );
                        None
                    }
                    Err(RecallLoadError::Context(RecallContextError::ThreadMismatch {
                        ..
                    })) => {
                        return Err(self
                            .block_post_compact_recovery(
                                PostCompactRecoveryFailureClass::ThreadMismatch,
                            )
                            .await);
                    }
                    Err(RecallLoadError::Context(RecallContextError::Build(error))) => {
                        warn!(
                            %error,
                            "post-compact recall could not be reconstructed; injecting boundary only"
                        );
                        None
                    }
                };
                let packet = match PostCompactRecoveryContext::new(
                    &identity.compaction_window_id,
                    &identity.boundary_item_id,
                    recall.as_ref(),
                ) {
                    Ok(packet) => packet,
                    Err(error) if recall.is_some() => {
                        warn!(
                            %error,
                            "bounded post-compact recall packet unavailable; injecting boundary only"
                        );
                        match PostCompactRecoveryContext::new(
                            &identity.compaction_window_id,
                            &identity.boundary_item_id,
                            None,
                        ) {
                            Ok(packet) => packet,
                            Err(error) => {
                                return Err(self
                                    .block_post_compact_recovery(context_failure_class(&error))
                                    .await);
                            }
                        }
                    }
                    Err(error) => {
                        return Err(self
                            .block_post_compact_recovery(context_failure_class(&error))
                            .await);
                    }
                };
                let cache_result = {
                    let mut state = self.state.lock().await;
                    state
                        .post_compact_recovery
                        .cache_packet(&identity, packet.clone())
                };
                if let Err(failure) = cache_result {
                    return Err(self.block_post_compact_recovery(failure).await);
                }
                packet
            }
        };

        let mut boundary_index = None;
        for (index, item) in input.iter().enumerate() {
            if item
                .id()
                .is_some_and(|item_id| item_id.as_str() == identity.boundary_item_id.as_str())
            {
                if boundary_index.replace(index).is_some() {
                    return Err(self
                        .block_post_compact_recovery(
                            PostCompactRecoveryFailureClass::BoundaryMismatch,
                        )
                        .await);
                }
            }
        }
        let Some(boundary_index) = boundary_index else {
            return Err(self
                .block_post_compact_recovery(PostCompactRecoveryFailureClass::BoundaryMismatch)
                .await);
        };
        let insertion_index = boundary_index + 1;
        let recall = packet.recall().cloned();
        input.insert(insertion_index, Box::new(packet).into_boxed_response_item());
        let item_count = if let Some(recall) = recall {
            input.insert(
                insertion_index + 1,
                Box::new(recall).into_boxed_response_item(),
            );
            2
        } else {
            1
        };
        Ok(Some(PreparedPostCompactRecovery {
            identity,
            insertion_index,
            item_count,
        }))
    }

    pub(super) async fn record_post_compact_recovery_sampling_success(
        &self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
    ) -> CodexResult<()> {
        let application = {
            let state = self.state.lock().await;
            state
                .post_compact_recovery
                .application_for_sampling_success(identity, turn_id)
        };
        let application = match application {
            Ok(application) => application,
            Err(failure) => return Err(self.block_post_compact_recovery(failure).await),
        };
        let live_thread = match self
            .live_thread_for_persistence("append the post-compact recovery application proof")
        {
            Ok(live_thread) => live_thread,
            Err(error) => {
                warn!(
                    %error,
                    "post-compact recovery application was not persisted; leaving it pending"
                );
                return Ok(());
            }
        };
        if let Err(error) = live_thread
            .append_items_durably(&[RolloutItem::PostCompactRecoveryApplied(application.clone())])
            .await
        {
            warn!(
                %error,
                "post-compact recovery application was not persisted; leaving it pending"
            );
            return Ok(());
        }
        let mut state = self.state.lock().await;
        if let Err(failure) = state
            .post_compact_recovery
            .clear_after_application(&application)
        {
            state.post_compact_recovery.block(failure);
            return Err(CodexErr::Fatal(format!(
                "durable post-compact recovery proof no longer matches live state: {failure}"
            )));
        }
        Ok(())
    }

    async fn block_post_compact_recovery(
        &self,
        failure: PostCompactRecoveryFailureClass,
    ) -> CodexErr {
        let mut state = self.state.lock().await;
        state.post_compact_recovery.block(failure);
        blocked_error(failure)
    }
}

fn context_failure_class(
    error: &PostCompactRecoveryContextError,
) -> PostCompactRecoveryFailureClass {
    match error {
        PostCompactRecoveryContextError::RecallParse(_) => {
            PostCompactRecoveryFailureClass::RecallParse
        }
        PostCompactRecoveryContextError::Serialization(_) => {
            PostCompactRecoveryFailureClass::Serialization
        }
        PostCompactRecoveryContextError::PacketCap => PostCompactRecoveryFailureClass::PacketCap,
    }
}

fn blocked_error(failure: PostCompactRecoveryFailureClass) -> CodexErr {
    CodexErr::Fatal(format!("post-compact recovery is blocked: {failure}"))
}
