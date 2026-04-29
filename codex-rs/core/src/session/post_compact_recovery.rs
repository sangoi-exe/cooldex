use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_analytics::now_unix_seconds;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::DeveloperInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::PostCompactRecoveryCompactionAnchor;
use codex_protocol::protocol::PostCompactRecoveryItem;
use codex_protocol::protocol::PostCompactRecoveryStatus;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::WarningEvent;
use serde_json::Value;
use serde_json::json;
use tracing::debug;
use tracing::warn;

use crate::function_tool::FunctionCallError;
use crate::prompt_gc_rollout::is_private_prompt_gc_compaction_marker;
use crate::rollout::RolloutRecorder;
use crate::session::post_compact_recovery_replay::RecoveryLifecycle;
use crate::session::post_compact_recovery_replay::compaction_anchor;
use crate::session::post_compact_recovery_replay::compaction_anchor_at_index;
use crate::session::post_compact_recovery_replay::replay_post_compact_recovery;
use crate::session::rollout_recovery_enabled;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::handlers::recall::build_recall_payload;

// Merge-safety anchor: post-compact runtime recovery is a typed session-owned
// lifecycle, not model-visible user task content or warning-text protocol.
#[derive(Clone, Debug)]
pub(crate) enum PendingPostCompactRecovery {
    Ready {
        item: Box<PostCompactRecoveryItem>,
        injected: bool,
    },
    Unavailable {
        packet: String,
        injected: bool,
    },
}

impl PendingPostCompactRecovery {
    pub(crate) fn ready(item: PostCompactRecoveryItem) -> Self {
        Self::Ready {
            item: Box::new(item),
            injected: false,
        }
    }

    fn unavailable(packet: String) -> Self {
        Self::Unavailable {
            packet,
            injected: false,
        }
    }

    pub(crate) fn packet(&self) -> Option<&str> {
        match self {
            Self::Ready { item, .. } => item.packet.as_deref(),
            Self::Unavailable { packet, .. } => Some(packet.as_str()),
        }
    }

    pub(crate) fn mark_injected(&mut self) {
        match self {
            Self::Ready { injected, .. } | Self::Unavailable { injected, .. } => {
                *injected = true;
            }
        }
    }

    pub(crate) fn injected(&self) -> bool {
        match self {
            Self::Ready { injected, .. } | Self::Unavailable { injected, .. } => *injected,
        }
    }

    fn cleared_item(&self) -> Option<PostCompactRecoveryItem> {
        let Self::Ready { item, .. } = self else {
            return None;
        };
        let mut cleared_item = item.as_ref().clone();
        cleared_item.status = PostCompactRecoveryStatus::Cleared;
        cleared_item.created_at_unix_secs = Some(now_unix_seconds());
        cleared_item.packet = None;
        cleared_item.failure = None;
        Some(cleared_item)
    }
}

#[derive(Debug)]
struct RecoveryPacketBuild {
    packet: String,
    compaction_anchor: PostCompactRecoveryCompactionAnchor,
    latest_compacted_index: Option<usize>,
    last_boundary_kind: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum UnavailableReason {
    RolloutRecoveryUnavailable,
    PreparationInterrupted,
    PreparationFailed,
    ResumeRepairFailed,
}

impl UnavailableReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::RolloutRecoveryUnavailable => "rollout_recovery_unavailable",
            Self::PreparationInterrupted => "preparation_interrupted",
            Self::PreparationFailed => "preparation_failed",
            Self::ResumeRepairFailed => "resume_repair_failed",
        }
    }
}

impl Session {
    pub(crate) async fn begin_post_compact_recovery(
        &self,
        turn_context: &TurnContext,
        reason: CompactionReason,
        phase: CompactionPhase,
        implementation: &'static str,
        warning: String,
    ) -> CodexResult<()> {
        if !rollout_recovery_enabled(turn_context.config.as_ref()) {
            let packet = unavailable_recovery_packet(
                UnavailableReason::RolloutRecoveryUnavailable,
                None,
                None,
            );
            {
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(
                    PendingPostCompactRecovery::unavailable(packet),
                ));
            }
            self.emit_post_compact_recovery_warning(turn_context, warning)
                .await;
            return Ok(());
        }

        let recovery_sequence = {
            let mut state = self.state.lock().await;
            state.allocate_post_compact_recovery_sequence()
        };
        let created_at_unix_secs = now_unix_seconds();
        let recovery_id = format!(
            "{}:{}:recovery-{}",
            turn_context.sub_id,
            compaction_phase_name(phase),
            recovery_sequence
        );
        let compaction_anchor = match self.latest_rollout_compaction_anchor().await {
            Ok(anchor) => anchor,
            Err(error) => {
                warn!(
                    recovery_id,
                    error = %error,
                    "failed to precompute post-compact recovery compaction anchor"
                );
                None
            }
        };
        let started_item = PostCompactRecoveryItem {
            recovery_id: recovery_id.clone(),
            turn_id: turn_context.sub_id.clone(),
            status: PostCompactRecoveryStatus::Started,
            phase: compaction_phase_name(phase).to_string(),
            reason: compaction_reason_name(reason).to_string(),
            implementation: implementation.to_string(),
            compaction_anchor,
            latest_compacted_index: None,
            last_boundary_kind: None,
            created_at_unix_secs: Some(created_at_unix_secs),
            packet: None,
            failure: None,
        };

        self.persist_post_compact_recovery_item(&started_item)
            .await?;

        let packet_build_result = self
            .build_post_compact_recovery_packet(turn_context, &started_item)
            .await;
        let packet_build = match packet_build_result {
            Ok(packet_build) => packet_build,
            Err(error) => {
                let mut failed_item = started_item.clone();
                failed_item.status = PostCompactRecoveryStatus::Failed;
                failed_item.created_at_unix_secs = Some(now_unix_seconds());
                failed_item.failure = Some(error.to_string());
                if let Err(persist_error) =
                    self.persist_post_compact_recovery_item(&failed_item).await
                {
                    warn!(
                        recovery_id,
                        error = %persist_error,
                        "failed to persist failed post-compact recovery item"
                    );
                }
                return Err(error);
            }
        };

        let mut ready_item = started_item;
        ready_item.status = PostCompactRecoveryStatus::Ready;
        ready_item.created_at_unix_secs = Some(now_unix_seconds());
        ready_item.compaction_anchor = Some(packet_build.compaction_anchor);
        ready_item.latest_compacted_index = packet_build.latest_compacted_index;
        ready_item.last_boundary_kind = packet_build.last_boundary_kind;
        ready_item.packet = Some(packet_build.packet);
        self.persist_post_compact_recovery_item(&ready_item).await?;

        {
            let mut state = self.state.lock().await;
            state.set_pending_post_compact_recovery(Some(PendingPostCompactRecovery::ready(
                ready_item,
            )));
        }

        self.emit_post_compact_recovery_warning(turn_context, warning)
            .await;
        Ok(())
    }

    pub(crate) async fn inject_pending_post_compact_recovery(
        &self,
        mut input: Vec<ResponseItem>,
    ) -> Vec<ResponseItem> {
        let packet = {
            let mut state = self.state.lock().await;
            let Some(recovery) = state.pending_post_compact_recovery_mut() else {
                return input;
            };
            let Some(packet) = recovery.packet().map(ToString::to_string) else {
                return input;
            };
            recovery.mark_injected();
            packet
        };

        input.insert(0, DeveloperInstructions::new(packet).into());
        input
    }

    pub(crate) async fn clear_pending_post_compact_recovery_after_successful_turn(
        &self,
    ) -> CodexResult<()> {
        let pending = {
            let mut state = self.state.lock().await;
            let should_clear = state
                .pending_post_compact_recovery()
                .is_some_and(PendingPostCompactRecovery::injected);
            if should_clear {
                state.clear_pending_post_compact_recovery()
            } else {
                None
            }
        };

        let Some(pending) = pending else {
            return Ok(());
        };
        let Some(cleared_item) = pending.cleared_item() else {
            return Ok(());
        };
        self.persist_post_compact_recovery_item(&cleared_item).await
    }

    pub(crate) async fn restore_post_compact_recovery_from_replay(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
        surviving_compaction_indices: &[usize],
        surviving_post_compact_recovery_indices: &[usize],
    ) {
        let replay = match replay_post_compact_recovery(
            rollout_items,
            surviving_compaction_indices,
            surviving_post_compact_recovery_indices,
        ) {
            Ok(replay) => replay,
            Err(error) => {
                warn!(error = %error, "failed to replay post-compact recovery state");
                let packet = unavailable_recovery_packet(
                    UnavailableReason::ResumeRepairFailed,
                    None,
                    Some(error.to_string()),
                );
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(
                    PendingPostCompactRecovery::unavailable(packet),
                ));
                return;
            }
        };
        let mut state = self.state.lock().await;
        state.ensure_next_post_compact_recovery_sequence_at_least(replay.next_sequence);
        drop(state);

        match replay.lifecycle {
            RecoveryLifecycle::Ready(item) => {
                debug!(
                    recovery_id = %item.recovery_id,
                    turn_id = %item.turn_id,
                    "restored pending post-compact recovery from rollout replay"
                );
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(PendingPostCompactRecovery::ready(
                    item,
                )));
            }
            RecoveryLifecycle::Cleared | RecoveryLifecycle::NoSurvivingCompaction => {
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(None);
            }
            RecoveryLifecycle::Started(item) => {
                let packet = unavailable_recovery_packet(
                    UnavailableReason::PreparationInterrupted,
                    Some(&item),
                    None,
                );
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(
                    PendingPostCompactRecovery::unavailable(packet),
                ));
            }
            RecoveryLifecycle::Failed(item) => {
                let packet = unavailable_recovery_packet(
                    UnavailableReason::PreparationFailed,
                    Some(&item),
                    item.failure.clone(),
                );
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(
                    PendingPostCompactRecovery::unavailable(packet),
                ));
            }
            RecoveryLifecycle::MissingForSurvivingCompaction(compaction_anchor) => {
                self.repair_missing_post_compact_recovery(turn_context, compaction_anchor)
                    .await;
            }
        }
    }

    async fn build_post_compact_recovery_packet(
        &self,
        turn_context: &TurnContext,
        base_item: &PostCompactRecoveryItem,
    ) -> CodexResult<RecoveryPacketBuild> {
        let recorder = self.post_compact_recovery_rollout_recorder().await?;
        recorder.flush().await.map_err(CodexErr::Io)?;
        let rollout_path = recorder.rollout_path().to_path_buf();
        let (rollout_items, _thread_id, parse_errors) =
            RolloutRecorder::load_rollout_items_skipping_malformed_lines(rollout_path.as_path())
                .await
                .map_err(CodexErr::Io)?;
        let recall_payload = build_recall_payload(
            &rollout_items,
            turn_context.config.recall_kbytes_limit,
            parse_errors,
            /*recall_debug*/ false,
        )
        .map_err(recall_packet_error)?;
        let latest_compacted_index = recall_payload
            .pointer("/boundary/latest_compacted_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        let compaction_anchor = match latest_compacted_index {
            Some(index) => compaction_anchor_at_index(&rollout_items, index)?,
            None => {
                return Err(CodexErr::Fatal(
                    "post-compact recovery packet has no compacted marker boundary".to_string(),
                ));
            }
        };
        if let Some(expected_anchor) = &base_item.compaction_anchor
            && expected_anchor != &compaction_anchor
        {
            return Err(CodexErr::Fatal(format!(
                "post-compact recovery compaction anchor mismatch: expected {expected_anchor:?}, got {compaction_anchor:?}"
            )));
        }
        let last_boundary_kind = recall_payload
            .pointer("/boundary/last_boundary_kind")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let packet_payload = json!({
            "mode": "post_compact_runtime_recovery",
            "recovery": {
                "id": &base_item.recovery_id,
                "turn_id": &base_item.turn_id,
                "phase": &base_item.phase,
                "reason": &base_item.reason,
                "implementation": &base_item.implementation,
                "compaction_anchor": &compaction_anchor,
                "latest_compacted_index": latest_compacted_index,
                "last_boundary_kind": last_boundary_kind,
                "created_at_unix_secs": base_item.created_at_unix_secs,
            },
            "runtime_obligations": {
                "do_not_dump_raw_recall_to_user": true,
                "inspect_live_state_if_continuity_is_unclear": true,
                "scratchpads_are_fallback_hints": true,
            },
            "recall": recall_payload,
        });
        let packet_json = serde_json::to_string(&packet_payload)?;
        Ok(RecoveryPacketBuild {
            packet: format!("<post_compact_recovery>\n{packet_json}\n</post_compact_recovery>"),
            compaction_anchor,
            latest_compacted_index,
            last_boundary_kind,
        })
    }

    async fn repair_missing_post_compact_recovery(
        &self,
        turn_context: &TurnContext,
        compaction_anchor: PostCompactRecoveryCompactionAnchor,
    ) {
        let recovery_sequence = {
            let mut state = self.state.lock().await;
            state.allocate_post_compact_recovery_sequence()
        };
        let recovery_id = format!(
            "{}:resume_repair:recovery-{}",
            turn_context.sub_id, recovery_sequence
        );
        let started_item = PostCompactRecoveryItem {
            recovery_id: recovery_id.clone(),
            turn_id: turn_context.sub_id.clone(),
            status: PostCompactRecoveryStatus::Started,
            phase: "resume_repair".to_string(),
            reason: "missing_ready_for_surviving_compaction".to_string(),
            implementation: "resume_repair".to_string(),
            compaction_anchor: Some(compaction_anchor),
            latest_compacted_index: None,
            last_boundary_kind: None,
            created_at_unix_secs: Some(now_unix_seconds()),
            packet: None,
            failure: None,
        };

        if let Err(error) = self.persist_post_compact_recovery_item(&started_item).await {
            warn!(
                recovery_id,
                error = %error,
                "failed to persist started post-compact recovery repair item"
            );
            let packet = unavailable_recovery_packet(
                UnavailableReason::ResumeRepairFailed,
                Some(&started_item),
                Some(error.to_string()),
            );
            let mut state = self.state.lock().await;
            state.set_pending_post_compact_recovery(Some(PendingPostCompactRecovery::unavailable(
                packet,
            )));
            return;
        }

        match self
            .build_post_compact_recovery_packet(turn_context, &started_item)
            .await
        {
            Ok(packet_build) => {
                let mut ready_item = started_item;
                ready_item.status = PostCompactRecoveryStatus::Ready;
                ready_item.created_at_unix_secs = Some(now_unix_seconds());
                ready_item.compaction_anchor = Some(packet_build.compaction_anchor);
                ready_item.latest_compacted_index = packet_build.latest_compacted_index;
                ready_item.last_boundary_kind = packet_build.last_boundary_kind;
                ready_item.packet = Some(packet_build.packet);
                if let Err(error) = self.persist_post_compact_recovery_item(&ready_item).await {
                    warn!(
                        recovery_id,
                        error = %error,
                        "failed to persist ready post-compact recovery repair item"
                    );
                    let packet = unavailable_recovery_packet(
                        UnavailableReason::ResumeRepairFailed,
                        Some(&ready_item),
                        Some(error.to_string()),
                    );
                    let mut state = self.state.lock().await;
                    state.set_pending_post_compact_recovery(Some(
                        PendingPostCompactRecovery::unavailable(packet),
                    ));
                    return;
                }
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(PendingPostCompactRecovery::ready(
                    ready_item,
                )));
            }
            Err(error) => {
                let mut failed_item = started_item;
                failed_item.status = PostCompactRecoveryStatus::Failed;
                failed_item.created_at_unix_secs = Some(now_unix_seconds());
                failed_item.failure = Some(error.to_string());
                if let Err(persist_error) =
                    self.persist_post_compact_recovery_item(&failed_item).await
                {
                    warn!(
                        recovery_id,
                        error = %persist_error,
                        "failed to persist failed post-compact recovery repair item"
                    );
                }
                let packet = unavailable_recovery_packet(
                    UnavailableReason::ResumeRepairFailed,
                    Some(&failed_item),
                    failed_item.failure.clone(),
                );
                let mut state = self.state.lock().await;
                state.set_pending_post_compact_recovery(Some(
                    PendingPostCompactRecovery::unavailable(packet),
                ));
            }
        }
    }

    async fn latest_rollout_compaction_anchor(
        &self,
    ) -> CodexResult<Option<PostCompactRecoveryCompactionAnchor>> {
        let recorder = self.post_compact_recovery_rollout_recorder().await?;
        recorder.flush().await.map_err(CodexErr::Io)?;
        let rollout_path = recorder.rollout_path().to_path_buf();
        let (rollout_items, _thread_id, _parse_errors) =
            RolloutRecorder::load_rollout_items_skipping_malformed_lines(rollout_path.as_path())
                .await
                .map_err(CodexErr::Io)?;
        for (index, item) in rollout_items.iter().enumerate().rev() {
            if let RolloutItem::Compacted(compacted) = item
                && !is_private_prompt_gc_compaction_marker(compacted)
            {
                return Ok(Some(compaction_anchor(index, compacted)?));
            }
        }
        Ok(None)
    }

    async fn persist_post_compact_recovery_item(
        &self,
        item: &PostCompactRecoveryItem,
    ) -> CodexResult<()> {
        let recorder = self.post_compact_recovery_rollout_recorder().await?;
        recorder
            .persist_items_atomically(&[RolloutItem::PostCompactRecovery(item.clone())])
            .await
            .map_err(CodexErr::Io)
    }

    async fn post_compact_recovery_rollout_recorder(&self) -> CodexResult<RolloutRecorder> {
        let recorder = {
            let guard = self.services.rollout.lock().await;
            guard.clone()
        };
        recorder.ok_or_else(|| {
            CodexErr::Fatal("post-compact recovery requires an active rollout recorder".to_string())
        })
    }

    async fn emit_post_compact_recovery_warning(
        &self,
        turn_context: &TurnContext,
        warning: String,
    ) {
        self.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent { message: warning }),
        )
        .await;
    }
}

fn unavailable_recovery_packet(
    reason: UnavailableReason,
    item: Option<&PostCompactRecoveryItem>,
    failure: Option<String>,
) -> String {
    let failure_summary = failure.and_then(|failure| {
        let trimmed = failure.trim();
        if trimmed.is_empty() {
            None
        } else if trimmed.len() > 500 {
            let summary = trimmed.chars().take(500).collect::<String>();
            Some(format!("{summary}..."))
        } else {
            Some(trimmed.to_string())
        }
    });
    let recovery_payload = match item {
        Some(item) => json!({
            "id": &item.recovery_id,
            "turn_id": &item.turn_id,
            "phase": &item.phase,
            "reason": &item.reason,
            "implementation": &item.implementation,
            "status": &item.status,
            "compaction_anchor": &item.compaction_anchor,
            "latest_compacted_index": item.latest_compacted_index,
            "last_boundary_kind": &item.last_boundary_kind,
            "created_at_unix_secs": item.created_at_unix_secs,
            "unavailable_reason": reason.as_str(),
            "failure_summary": failure_summary,
        }),
        None => json!({
            "unavailable_reason": reason.as_str(),
            "failure_summary": failure_summary,
        }),
    };
    let packet_payload = json!({
        "mode": "post_compact_runtime_recovery_unavailable",
        "recovery": recovery_payload,
        "runtime_obligations": {
            "do_not_claim_recall_was_available": true,
            "inspect_live_state_before_continuing": true,
            "state_exact_missing_context_if_unclear": true,
        },
    });
    let packet_json = packet_payload.to_string();
    format!("<post_compact_recovery>\n{packet_json}\n</post_compact_recovery>")
}

fn recall_packet_error(error: FunctionCallError) -> CodexErr {
    CodexErr::Fatal(format!(
        "failed to build post-compact recall packet: {error}"
    ))
}

fn compaction_reason_name(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::UserRequested => "user_requested",
        CompactionReason::ContextLimit => "context_limit",
        CompactionReason::ModelDownshift => "model_downshift",
    }
}

fn compaction_phase_name(phase: CompactionPhase) -> &'static str {
    match phase {
        CompactionPhase::StandaloneTurn => "standalone_turn",
        CompactionPhase::PreTurn => "pre_turn",
        CompactionPhase::MidTurn => "mid_turn",
    }
}
