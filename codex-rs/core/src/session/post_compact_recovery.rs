use std::io;

use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;

use super::Session;
use super::TurnContext;
use super::recall::RecallContextError;
use super::recall::RecallLoadError;
use crate::context::ContextualUserFragment;
use crate::context::PostCompactRecoveryContext;
use crate::context::PostCompactRecoveryContextError;
use crate::state::PostCompactRecoveryFailureClass;
use crate::state::PostCompactRecoveryIdentity;

#[derive(Debug)]
pub(super) struct PreparedPostCompactRecovery {
    identity: PostCompactRecoveryIdentity,
    insertion_index: usize,
}

impl PreparedPostCompactRecovery {
    pub(super) fn identity(&self) -> &PostCompactRecoveryIdentity {
        &self.identity
    }

    pub(super) fn remove_from_input(
        self,
        input: &mut Vec<ResponseItem>,
    ) -> CodexResult<()> {
        let is_recovery_item = input.get(self.insertion_index).is_some_and(|item| {
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
        if !is_recovery_item {
            return Err(CodexErr::Fatal(
                "post-compact recovery prompt carrier lost its transient item".to_string(),
            ));
        }
        input.remove(self.insertion_index);
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
                let recall = match self
                    .load_current_thread_recall_context(turn_context)
                    .await
                {
                    Ok(recall) => recall,
                    Err(RecallLoadError::Source(error)) => {
                        return Err(CodexErr::Io(io::Error::other(error.to_string())));
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
                    Err(RecallLoadError::Context(RecallContextError::Build(_))) => {
                        return Err(self
                            .block_post_compact_recovery(
                                PostCompactRecoveryFailureClass::RecallParse,
                            )
                            .await);
                    }
                };
                let packet = match PostCompactRecoveryContext::new(
                    &identity.compaction_window_id,
                    &identity.boundary_item_id,
                    &recall,
                ) {
                    Ok(packet) => packet,
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
        input.insert(
            insertion_index,
            Box::new(packet).into_boxed_response_item(),
        );
        Ok(Some(PreparedPostCompactRecovery {
            identity,
            insertion_index,
        }))
    }

    pub(super) async fn record_post_compact_recovery_sampling_success(
        &self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
    ) -> CodexResult<()> {
        let result = {
            let mut state = self.state.lock().await;
            state
                .post_compact_recovery
                .record_sampling_success(identity, turn_id)
        };
        match result {
            Ok(()) => Ok(()),
            Err(failure) => Err(self.block_post_compact_recovery(failure).await),
        }
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
        PostCompactRecoveryContextError::PacketCap => {
            PostCompactRecoveryFailureClass::PacketCap
        }
    }
}

fn blocked_error(failure: PostCompactRecoveryFailureClass) -> CodexErr {
    CodexErr::Fatal(format!("post-compact recovery is blocked: {failure}"))
}
