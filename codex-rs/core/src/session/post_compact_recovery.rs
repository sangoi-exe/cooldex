use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_analytics::now_unix_seconds;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::DeveloperInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::PostCompactRecoveryItem;
use codex_protocol::protocol::PostCompactRecoveryStatus;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::WarningEvent;
use serde_json::Value;
use serde_json::json;
use tracing::debug;
use tracing::warn;

use crate::function_tool::FunctionCallError;
use crate::rollout::RolloutRecorder;
use crate::session::rollout_recovery_enabled;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::handlers::recall::build_recall_payload;

// Merge-safety anchor: post-compact runtime recovery is a typed session-owned
// lifecycle, not model-visible user task content or warning-text protocol.
#[derive(Clone, Debug)]
pub(crate) struct PendingPostCompactRecovery {
    item: PostCompactRecoveryItem,
    injected: bool,
}

impl PendingPostCompactRecovery {
    pub(crate) fn new(item: PostCompactRecoveryItem) -> Self {
        Self {
            item,
            injected: false,
        }
    }

    pub(crate) fn packet(&self) -> Option<&str> {
        self.item.packet.as_deref()
    }

    pub(crate) fn mark_injected(&mut self) {
        self.injected = true;
    }

    pub(crate) fn injected(&self) -> bool {
        self.injected
    }

    fn cleared_item(&self) -> PostCompactRecoveryItem {
        let mut item = self.item.clone();
        item.status = PostCompactRecoveryStatus::Cleared;
        item.created_at_unix_secs = Some(now_unix_seconds());
        item.packet = None;
        item.failure = None;
        item
    }
}

#[derive(Debug)]
struct RecoveryPacketBuild {
    packet: String,
    latest_compacted_index: Option<usize>,
    last_boundary_kind: Option<String>,
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
        let started_item = PostCompactRecoveryItem {
            recovery_id: recovery_id.clone(),
            turn_id: turn_context.sub_id.clone(),
            status: PostCompactRecoveryStatus::Started,
            phase: compaction_phase_name(phase).to_string(),
            reason: compaction_reason_name(reason).to_string(),
            implementation: implementation.to_string(),
            latest_compacted_index: None,
            last_boundary_kind: None,
            created_at_unix_secs: Some(created_at_unix_secs),
            packet: None,
            failure: None,
        };

        self.persist_post_compact_recovery_item(&started_item)
            .await?;

        let packet_build_result = self
            .build_post_compact_recovery_packet(
                turn_context,
                &started_item,
                reason,
                phase,
                implementation,
            )
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
        ready_item.latest_compacted_index = packet_build.latest_compacted_index;
        ready_item.last_boundary_kind = packet_build.last_boundary_kind;
        ready_item.packet = Some(packet_build.packet);
        self.persist_post_compact_recovery_item(&ready_item).await?;

        {
            let mut state = self.state.lock().await;
            state.set_pending_post_compact_recovery(Some(PendingPostCompactRecovery::new(
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
        self.persist_post_compact_recovery_item(&pending.cleared_item())
            .await
    }

    pub(crate) async fn restore_post_compact_recovery_from_rollout(
        &self,
        rollout_items: &[RolloutItem],
    ) {
        let pending = latest_ready_post_compact_recovery(rollout_items);
        let next_sequence = next_post_compact_recovery_sequence(rollout_items);
        if let Some(item) = &pending {
            debug!(
                recovery_id = %item.recovery_id,
                turn_id = %item.turn_id,
                "restored pending post-compact recovery from rollout"
            );
        }
        let mut state = self.state.lock().await;
        state.ensure_next_post_compact_recovery_sequence_at_least(next_sequence);
        state.set_pending_post_compact_recovery(pending.map(PendingPostCompactRecovery::new));
    }

    async fn build_post_compact_recovery_packet(
        &self,
        turn_context: &TurnContext,
        base_item: &PostCompactRecoveryItem,
        reason: CompactionReason,
        phase: CompactionPhase,
        implementation: &'static str,
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
        let last_boundary_kind = recall_payload
            .pointer("/boundary/last_boundary_kind")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let packet_payload = json!({
            "mode": "post_compact_runtime_recovery",
            "recovery": {
                "id": &base_item.recovery_id,
                "turn_id": &base_item.turn_id,
                "phase": compaction_phase_name(phase),
                "reason": compaction_reason_name(reason),
                "implementation": implementation,
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
            latest_compacted_index,
            last_boundary_kind,
        })
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

fn latest_ready_post_compact_recovery(
    rollout_items: &[RolloutItem],
) -> Option<PostCompactRecoveryItem> {
    let mut pending = None;
    for item in rollout_items {
        let RolloutItem::PostCompactRecovery(item) = item else {
            continue;
        };
        match item.status {
            PostCompactRecoveryStatus::Ready if item.packet.is_some() => {
                pending = Some(item.clone());
            }
            PostCompactRecoveryStatus::Started
            | PostCompactRecoveryStatus::Ready
            | PostCompactRecoveryStatus::Failed
            | PostCompactRecoveryStatus::Cleared => {
                pending = None;
            }
        }
    }
    pending
}

fn next_post_compact_recovery_sequence(rollout_items: &[RolloutItem]) -> u64 {
    let mut next_sequence = 0_u64;
    for item in rollout_items {
        let RolloutItem::PostCompactRecovery(item) = item else {
            continue;
        };
        if let Some(sequence) = recovery_sequence_from_id(&item.recovery_id) {
            next_sequence = next_sequence.max(sequence.saturating_add(1));
        }
    }
    next_sequence
}

fn recovery_sequence_from_id(recovery_id: &str) -> Option<u64> {
    let (_prefix, sequence) = recovery_id.rsplit_once(":recovery-")?;
    sequence.parse().ok()
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
