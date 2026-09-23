use codex_history::RolloutItem;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use tracing::warn;

use super::Session;
use super::TurnContext;
use crate::context::ContextualUserFragment;
use crate::context::PostCompactHandoffContext;
use crate::context::PostCompactRecoveryContext;
use crate::context::PostCompactRecoveryContextError;
use crate::state::PostCompactRecoveryFailureClass;
use crate::state::PostCompactRecoveryIdentity;
use crate::tool_batch::ToolItemKind;
use crate::tool_batch::classify_tool_item;
use crate::tool_batch::complete_trailing_tool_batch;

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
        let has_handoff_item = self.item_count == 1
            || input.get(self.insertion_index).is_some_and(|item| {
                matches!(
                    item,
                    ResponseItem::Message { role, content, .. }
                        if role == "assistant"
                            && content.iter().any(|content| {
                                matches!(
                                    content,
                                    codex_protocol::models::ContentItem::OutputText { text }
                                        if PostCompactHandoffContext::matches_text(text)
                                )
                            })
                )
            });
        let recovery_index = self.insertion_index + usize::from(self.item_count == 2);
        let has_recovery_item = input.get(recovery_index).is_some_and(|item| {
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
        if !has_recovery_item || !has_handoff_item {
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
                let instructions = turn_context
                    .config
                    .post_compact_recovery_instructions
                    .as_deref()
                    .unwrap_or(PostCompactRecoveryContext::default_instructions());
                let packet = match PostCompactRecoveryContext::new(
                    &identity.compaction_window_id,
                    &identity.boundary_item_id,
                    instructions,
                    None,
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

        // Merge-safety anchor: normal sampling retains its current failure-to-block behavior;
        // the extracted placement helper is also used by a read-only pre-compaction snapshot.
        match insert_post_compact_recovery_packet(input, &identity, packet) {
            Ok(prepared) => Ok(Some(prepared)),
            Err(failure) => Err(self.block_post_compact_recovery(failure).await),
        }
    }

    #[cfg(test)]
    pub(crate) async fn record_post_compact_recovery_sampling_success(
        &self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
    ) -> CodexResult<()> {
        self.record_post_compact_recovery_sampling_success_inner(
            identity, turn_id, /*cancellation_token*/ None,
        )
        .await
    }

    /// Records a recovery proof only while the sampling task remains uncancelled after
    /// persistence-publication admission.
    pub(crate) async fn record_post_compact_recovery_sampling_success_for_task(
        &self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
        cancellation_token: &tokio_util::sync::CancellationToken,
    ) -> CodexResult<()> {
        self.record_post_compact_recovery_sampling_success_inner(
            identity,
            turn_id,
            Some(cancellation_token),
        )
        .await
    }

    async fn record_post_compact_recovery_sampling_success_inner(
        &self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
        cancellation_token: Option<&tokio_util::sync::CancellationToken>,
    ) -> CodexResult<()> {
        // Merge-safety anchor: retain the shared persistence-publication permit from the
        // application proof through the matching live clear, so retirement cannot cross it. A
        // retiring lifecycle cancels this task token before releasing the shared permit.
        let _persistence_guard = self.acquire_thread_settings_persistence().await;
        if cancellation_token.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Err(CodexErr::TurnAborted);
        }
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
                return Err(CodexErr::Fatal(format!(
                    "failed to persist post-compact recovery application proof: {error}"
                )));
            }
        };
        if let Err(error) = live_thread
            .append_items_durably(&[RolloutItem::PostCompactRecoveryApplied(application.clone())])
            .await
        {
            return Err(CodexErr::Fatal(format!(
                "failed to persist post-compact recovery application proof: {error}"
            )));
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

/// Inserts one already-materialized packet at the canonical recovery boundary without touching
/// session state. Callers own packet construction, caching, and failure-state transitions.
pub(super) fn insert_post_compact_recovery_packet(
    input: &mut Vec<ResponseItem>,
    identity: &PostCompactRecoveryIdentity,
    packet: PostCompactRecoveryContext,
) -> Result<PreparedPostCompactRecovery, PostCompactRecoveryFailureClass> {
    let mut boundary_index = None;
    for (index, item) in input.iter().enumerate() {
        if item
            .id()
            .is_some_and(|item_id| item_id.as_str() == identity.boundary_item_id.as_str())
            && boundary_index.replace(index).is_some()
        {
            return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
        }
    }
    let Some(boundary_index) = boundary_index else {
        return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
    };
    let insertion_index = match classify_tool_item(&input[boundary_index]) {
        ToolItemKind::Output => match complete_trailing_tool_batch(&input[..=boundary_index]) {
            Ok(batch) => batch.range.start,
            Err(reason) => {
                warn!(
                    %reason,
                    "post-compact recovery boundary ends an invalid native tool batch"
                );
                return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
            }
        },
        ToolItemKind::UnsupportedOutput => {
            return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
        }
        ToolItemKind::Call | ToolItemKind::UnsupportedCall | ToolItemKind::NonTool => {
            boundary_index + 1
        }
    };
    let handoff = packet.handoff().cloned();
    let compaction_window_id = &identity.compaction_window_id;
    let item_count = if let Some(handoff) = handoff {
        // Merge-safety anchor: replay the generated handoff as assistant output, not generic
        // contextual input.
        let mut handoff_item = handoff.into_response_item();
        handoff_item.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            format_args!("{compaction_window_id}-handoff"),
        )));
        let mut recovery_item = Box::new(packet).into_boxed_response_item();
        recovery_item.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            format_args!("{compaction_window_id}-recovery"),
        )));
        input.insert(insertion_index, handoff_item);
        input.insert(insertion_index + 1, recovery_item);
        2
    } else {
        let mut recovery_item = Box::new(packet).into_boxed_response_item();
        recovery_item.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            format_args!("{compaction_window_id}-recovery"),
        )));
        input.insert(insertion_index, recovery_item);
        1
    };
    Ok(PreparedPostCompactRecovery {
        identity: identity.clone(),
        insertion_index,
        item_count,
    })
}

fn context_failure_class(
    error: &PostCompactRecoveryContextError,
) -> PostCompactRecoveryFailureClass {
    match error {
        PostCompactRecoveryContextError::Serialization(_) => {
            PostCompactRecoveryFailureClass::Serialization
        }
        PostCompactRecoveryContextError::PacketCap => PostCompactRecoveryFailureClass::PacketCap,
    }
}

fn blocked_error(failure: PostCompactRecoveryFailureClass) -> CodexErr {
    CodexErr::Fatal(format!("post-compact recovery is blocked: {failure}"))
}
