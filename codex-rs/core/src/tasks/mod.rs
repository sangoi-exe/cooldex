mod compact;
mod lifecycle;
mod regular;
mod review;
mod user_shell;

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use codex_diagnostics::Gauge;
use codex_extension_api::ExtensionData;
use codex_extension_api::ThreadIdleCause;
use futures::future::BoxFuture;
use tokio::select;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::Span;
use tracing::field;
use tracing::info_span;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;

use crate::codex_thread::BackgroundTerminalInfo;
use crate::codex_thread::TryStartTurnIfIdleRejectionReason;
use crate::config::Config;
use crate::context::ContextualUserFragment;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn_context::TurnContext;
use crate::state::PostCompactRecoveryIdentity;
use crate::state::RetiredTurn;
use crate::state::RunningTask;
use crate::state::SteerAdmission;
use crate::state::TaskKind;
use crate::state::TerminalTransitionKind;
use crate::state::TurnSlotError;
use crate::state::TurnStartClaim;
use codex_analytics::TurnProfileFact;
use codex_analytics::TurnTokenUsageFact;
use codex_otel::SessionTelemetry;
use codex_otel::TURN_E2E_DURATION_METRIC;
use codex_otel::TURN_MEMORY_METRIC;
use codex_otel::TURN_NETWORK_PROXY_METRIC;
use codex_otel::TURN_TOKEN_USAGE_METRIC;
use codex_otel::TURN_TOOL_CALL_METRIC;
use codex_otel::TURN_UNIFIED_EXEC_RUNNING_PROCESSES_METRIC;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::WarningEvent;

use codex_features::Feature;
use codex_login::AuthManager;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
pub(crate) use compact::CompactTask;
pub(crate) use regular::RegularTask;
pub(crate) use review::ReviewTask;
pub(crate) use user_shell::UserShellCommandMode;
pub(crate) use user_shell::UserShellCommandTask;
pub(crate) use user_shell::execute_user_shell_command;

const GRACEFULL_INTERRUPTION_TIMEOUT_MS: u64 = 100;
const TASK_COMPACT_METRIC: &str = "codex.task.compact";
static ACTIVE_TURNS: Gauge = Gauge::new("core.turns.active");

fn turn_slot_codex_error(error: TurnSlotError) -> CodexErr {
    CodexErr::Fatal(format!("turn-slot invariant violation: {error}"))
}

fn terminal_transition_kind(reason: &TurnAbortReason) -> TerminalTransitionKind {
    match reason {
        TurnAbortReason::Replaced => TerminalTransitionKind::Replacing,
        TurnAbortReason::Interrupted
        | TurnAbortReason::ReviewEnded
        | TurnAbortReason::BudgetLimited => TerminalTransitionKind::Interrupting,
    }
}

enum AbortSlotAction {
    Noop,
    Wait(tokio::sync::watch::Receiver<u64>),
    Retire(RetiredTurn),
}

#[derive(Debug, Default)]
pub(crate) struct SessionTaskOutput {
    pub(crate) last_agent_message: Option<String>,
    pub(crate) post_compact_recovery: Option<PostCompactRecoveryIdentity>,
}

pub(crate) type SessionTaskResult = CodexResult<SessionTaskOutput>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegularTaskContinuation {
    Continue,
    Sealed,
}

pub(crate) enum MailboxParentProvenance {
    Ignore,
    Attribute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptedTurnHistoryMarker {
    Disabled,
    ContextualUser,
    Developer,
}

impl InterruptedTurnHistoryMarker {
    pub(crate) fn from_config_and_version(
        config: &Config,
        multi_agent_version: MultiAgentVersion,
    ) -> Self {
        if !config.agent_interrupt_message_enabled {
            return Self::Disabled;
        }
        if multi_agent_version == MultiAgentVersion::V2 {
            Self::Developer
        } else {
            Self::ContextualUser
        }
    }
}

/// Shared model-visible marker used by both the real interrupt path and
/// interrupted fork snapshots.
pub(crate) fn interrupted_turn_history_marker(
    marker: InterruptedTurnHistoryMarker,
) -> Option<ResponseItem> {
    match marker {
        InterruptedTurnHistoryMarker::Disabled => None,
        InterruptedTurnHistoryMarker::ContextualUser => Some(ContextualUserFragment::into(
            crate::context::TurnAborted::new(crate::context::TurnAborted::INTERRUPTED_GUIDANCE),
        )),
        InterruptedTurnHistoryMarker::Developer => {
            let marker = crate::context::TurnAborted::new(
                crate::context::TurnAborted::INTERRUPTED_DEVELOPER_GUIDANCE,
            );
            Some(ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: marker.render(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            })
        }
    }
}

fn emit_turn_network_proxy_metric(
    session_telemetry: &SessionTelemetry,
    network_proxy_active: bool,
    tmp_mem: (&str, &str),
) {
    let active = if network_proxy_active {
        "true"
    } else {
        "false"
    };
    session_telemetry.counter(
        TURN_NETWORK_PROXY_METRIC,
        /*inc*/ 1,
        &[("active", active), tmp_mem],
    );
}

fn emit_turn_memory_metric(
    session_telemetry: &SessionTelemetry,
    feature_enabled: bool,
    config_enabled: bool,
    has_citations: bool,
) {
    let read_allowed = feature_enabled && config_enabled;
    session_telemetry.counter(
        TURN_MEMORY_METRIC,
        /*inc*/ 1,
        &[
            ("read_allowed", bool_tag(read_allowed)),
            ("feature_enabled", bool_tag(feature_enabled)),
            ("config_use_memories", bool_tag(config_enabled)),
            ("has_citations", bool_tag(has_citations)),
        ],
    );
}

pub(crate) fn emit_compact_metric(
    session_telemetry: &SessionTelemetry,
    compact_type: &'static str,
    manual: bool,
) {
    session_telemetry.counter(
        TASK_COMPACT_METRIC,
        /*inc*/ 1,
        &[("type", compact_type), ("manual", bool_tag(manual))],
    );
}

fn bool_tag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// Thin wrapper that exposes the parts of [`Session`] task runners need.
#[derive(Clone)]
pub(crate) struct SessionTaskContext {
    session: Arc<Session>,
    turn_extension_data: Arc<ExtensionData>,
}

impl SessionTaskContext {
    pub(crate) fn new(session: Arc<Session>, turn_extension_data: Arc<ExtensionData>) -> Self {
        Self {
            session,
            turn_extension_data,
        }
    }

    pub(crate) fn clone_session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }

    pub(crate) fn turn_extension_data(&self) -> Arc<ExtensionData> {
        Arc::clone(&self.turn_extension_data)
    }

    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.session.services.auth_manager)
    }

    pub(crate) fn models_manager(&self) -> SharedModelsManager {
        Arc::clone(&self.session.services.models_manager)
    }
}

async fn emit_standard_turn_started(session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
    let event = EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: ctx.sub_id.clone(),
        trace_id: ctx.trace_id.clone(),
        started_at: ctx.turn_timing_state.started_at_unix_secs().await,
        model_context_window: ctx.model_context_window(),
        collaboration_mode_kind: ctx.mode,
    });
    session
        .clone_session()
        .send_event(ctx.as_ref(), event)
        .await;
}

/// Async task that drives a [`Session`] turn.
///
/// Implementations encapsulate a specific Codex workflow (regular chat,
/// reviews, ghost snapshots, etc.). Each task instance is owned by a
/// [`Session`] and executed on a background Tokio task. The trait is
/// intentionally small: implementers identify themselves via
/// [`SessionTask::kind`], perform their work in [`SessionTask::run`], and may
/// release resources in [`SessionTask::abort`].
pub(crate) trait SessionTask: Send + Sync + 'static {
    /// Describes the type of work the task performs so the session can
    /// surface it in telemetry and UI.
    fn kind(&self) -> TaskKind;

    /// Returns the tracing name for a spawned task span.
    fn span_name(&self) -> &'static str;

    /// Emits any task-specific protocol-visible turn-start event before steering opens.
    ///
    /// Tasks opt in when their existing protocol includes `TurnStarted`. The
    /// startup barrier still applies to tasks that intentionally emit no event.
    fn emit_turn_started(
        &self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let _ = (session, ctx);
        }
    }

    /// Executes the task until completion or cancellation.
    ///
    /// Implementations typically stream protocol events using `session` and
    /// `ctx`, returning task completion output when finished. The
    /// provided `cancellation_token` is cancelled when the session requests an
    /// abort; implementers should watch for it and terminate quickly once it
    /// fires. A populated [`SessionTaskOutput::last_agent_message`] is emitted
    /// to the client by [`Session::on_task_finished`]. Returning
    /// [`CodexErr::TurnAborted`] completes the task through the aborted-turn
    /// lifecycle instead.
    fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = SessionTaskResult> + Send;

    /// Gives the task a chance to perform cleanup after an abort.
    ///
    /// The default implementation is a no-op; override this if additional
    /// teardown or notifications are required once
    /// [`Session::abort_all_tasks`] cancels the task.
    fn abort(
        &self,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let _ = (session, ctx);
        }
    }
}

pub(crate) trait AnySessionTask: Send + Sync + 'static {
    fn kind(&self) -> TaskKind;

    fn span_name(&self) -> &'static str;

    fn emit_turn_started<'a>(
        &'a self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> BoxFuture<'a, ()>;

    fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> BoxFuture<'static, SessionTaskResult>;

    fn abort<'a>(&'a self, session: Arc<Session>, ctx: Arc<TurnContext>) -> BoxFuture<'a, ()>;
}

impl<T> AnySessionTask for T
where
    T: SessionTask,
{
    fn kind(&self) -> TaskKind {
        SessionTask::kind(self)
    }

    fn span_name(&self) -> &'static str {
        SessionTask::span_name(self)
    }

    fn emit_turn_started<'a>(
        &'a self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(SessionTask::emit_turn_started(self, session, ctx))
    }

    fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> BoxFuture<'static, SessionTaskResult> {
        Box::pin(SessionTask::run(
            self,
            session,
            ctx,
            input,
            cancellation_token,
        ))
    }

    fn abort<'a>(&'a self, session: Arc<Session>, ctx: Arc<TurnContext>) -> BoxFuture<'a, ()> {
        Box::pin(SessionTask::abort(self, session, ctx))
    }
}

impl Session {
    pub async fn spawn_task<T: SessionTask>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: T,
    ) {
        let session = Arc::clone(self);
        let task: Arc<dyn AnySessionTask> = Arc::new(task);
        let transition = tokio::spawn(
            async move {
                session
                    .replace_or_start_task(
                        turn_context,
                        input,
                        task,
                        None,
                        MailboxParentProvenance::Ignore,
                    )
                    .await
            }
            .instrument(Span::current()),
        );
        match transition.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(%err, "failed to replace or start session task"),
            Err(err) => warn!(%err, "turn-slot transition task failed"),
        }
    }

    async fn replace_or_start_task(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: Arc<dyn AnySessionTask>,
        input_persisted: Option<
            tokio::sync::oneshot::Sender<Result<(), TryStartTurnIfIdleRejectionReason>>,
        >,
        mailbox_parent_provenance: MailboxParentProvenance,
    ) -> CodexResult<()> {
        enum StartAction {
            Start(TurnStartClaim),
            Replace(RetiredTurn),
            Wait(tokio::sync::watch::Receiver<u64>),
        }

        loop {
            let action = {
                let mut slot = self.active_turn.lock().await;
                if slot.is_idle() {
                    StartAction::Start(
                        slot.claim_start(turn_context.sub_id.clone())
                            .map_err(turn_slot_codex_error)?,
                    )
                } else if slot
                    .running_task()
                    .is_some_and(|task| task.steer_admission == SteerAdmission::Starting)
                    || slot.is_starting_or_transitioning()
                {
                    StartAction::Wait(slot.subscribe_generation())
                } else {
                    StartAction::Replace(
                        slot.begin_transition(
                            TerminalTransitionKind::Replacing,
                            Some(turn_context.sub_id.clone()),
                        )
                        .map_err(turn_slot_codex_error)?,
                    )
                }
            };

            match action {
                StartAction::Start(claim) => {
                    self.clear_connector_selection().await;
                    return self
                        .start_claimed_task(
                            claim,
                            turn_context,
                            input,
                            task,
                            input_persisted,
                            mailbox_parent_provenance,
                        )
                        .await;
                }
                StartAction::Replace(retired_turn) => {
                    let transition_generation = retired_turn.transition_generation;
                    self.abort_retired_turn(retired_turn, TurnAbortReason::Replaced)
                        .await;
                    self.clear_connector_selection().await;
                    let claim = {
                        let mut slot = self.active_turn.lock().await;
                        slot.prepare_successor_start(
                            transition_generation,
                            turn_context.sub_id.clone(),
                        )
                        .map_err(turn_slot_codex_error)?
                    };
                    return self
                        .start_claimed_task(
                            claim,
                            turn_context,
                            input,
                            task,
                            input_persisted,
                            mailbox_parent_provenance,
                        )
                        .await;
                }
                StartAction::Wait(mut generation_rx) => {
                    generation_rx.changed().await.map_err(|_| {
                        CodexErr::Fatal(
                            "turn-slot generation channel closed during task startup".to_string(),
                        )
                    })?;
                }
            }
        }
    }

    pub(crate) async fn start_claimed_regular_task(
        self: &Arc<Self>,
        claim: TurnStartClaim,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
    ) -> CodexResult<()> {
        self.start_claimed_regular_task_with_options(
            claim,
            turn_context,
            input,
            None,
            MailboxParentProvenance::Ignore,
        )
        .await
    }

    pub(crate) async fn start_claimed_regular_task_with_options(
        self: &Arc<Self>,
        claim: TurnStartClaim,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        input_persisted: Option<
            tokio::sync::oneshot::Sender<Result<(), TryStartTurnIfIdleRejectionReason>>,
        >,
        mailbox_parent_provenance: MailboxParentProvenance,
    ) -> CodexResult<()> {
        self.start_claimed_task(
            claim,
            turn_context,
            input,
            Arc::new(RegularTask::new()),
            input_persisted,
            mailbox_parent_provenance,
        )
        .await
    }

    pub(crate) async fn start_task<T: SessionTask>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: T,
        input_persisted: Option<
            tokio::sync::oneshot::Sender<Result<(), TryStartTurnIfIdleRejectionReason>>,
        >,
        mailbox_parent_provenance: MailboxParentProvenance,
    ) {
        let task: Arc<dyn AnySessionTask> = Arc::new(task);
        if let Err(err) = self
            .replace_or_start_task(
                turn_context,
                input,
                task,
                input_persisted,
                mailbox_parent_provenance,
            )
            .await
        {
            warn!(%err, "failed to replace or start session task");
        }
    }

    async fn start_claimed_task(
        self: &Arc<Self>,
        claim: TurnStartClaim,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: Arc<dyn AnySessionTask>,
        input_persisted: Option<
            tokio::sync::oneshot::Sender<Result<(), TryStartTurnIfIdleRejectionReason>>,
        >,
        mailbox_parent_provenance: MailboxParentProvenance,
    ) -> CodexResult<()> {
        if claim.target_turn_id != turn_context.sub_id {
            self.cancel_claimed_start(&claim).await;
            return Err(CodexErr::Fatal(format!(
                "turn-slot start claim targets {}, but task context targets {}",
                claim.target_turn_id, turn_context.sub_id
            )));
        }
        let task_kind = task.kind();
        let span_name = task.span_name();
        let started_at = Instant::now();
        let turn_started_at_unix_ms = turn_context
            .turn_timing_state
            .mark_turn_started(started_at)
            .await;
        turn_context
            .turn_metadata_state
            .set_turn_started_at_unix_ms(turn_started_at_unix_ms);
        let token_usage_at_turn_start = self.total_token_usage().await.unwrap_or_default();

        let cancellation_token = CancellationToken::new();
        let task_done = Arc::new(Notify::new());
        let (start_tx, start_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();

        self.services
            .guardian_rejection_circuit_breaker
            .lock()
            .await
            .clear_turn(&turn_context.sub_id);

        let (pending_items, parent_turn_id) =
            self.input_queue.get_pending_input(&self.active_turn).await;
        if let (MailboxParentProvenance::Attribute, Some(id)) =
            (mailbox_parent_provenance, parent_turn_id)
        {
            turn_context.turn_metadata_state.set_parent_turn_id(id);
        }
        let turn_state = Arc::clone(&claim.turn_state);
        turn_state.lock().await.token_usage_at_turn_start = token_usage_at_turn_start.clone();
        self.input_queue
            .extend_pending_input_for_turn_state(turn_state.as_ref(), pending_items)
            .await;

        let turn_extension_data = Arc::clone(&turn_context.extension_data);
        let agent_execution_guard = self.services.agent_control.execution_guard(
            turn_context.multi_agent_version,
            &turn_context.session_source,
        );
        let session = Arc::clone(self);
        let task_done_clone = Arc::clone(&task_done);
        let session_ctx = Arc::new(SessionTaskContext::new(
            Arc::clone(self),
            Arc::clone(&turn_extension_data),
        ));
        let session_ctx_for_start = Arc::clone(&session_ctx);
        let ctx = Arc::clone(&turn_context);
        let task_for_run = Arc::clone(&task);
        let task_for_start = Arc::clone(&task);
        let task_input = input;
        let task_cancellation_token = cancellation_token.child_token();
        // Task-owned turn spans keep a core-owned span open for the
        // full task lifecycle after the submission dispatch span ends.
        let reasoning_effort = turn_context.effective_reasoning_effort_for_tracing();
        let task_span = info_span!(
            "turn",
            otel.name = span_name,
            thread.id = %self.thread_id,
            turn.id = %turn_context.sub_id,
            model = %turn_context.model_info.slug,
            codex.turn.reasoning_effort = %reasoning_effort,
            codex.turn.token_usage.input_tokens = field::Empty,
            codex.turn.token_usage.cached_input_tokens = field::Empty,
            codex.turn.token_usage.cache_write_input_tokens = field::Empty,
            codex.turn.token_usage.non_cached_input_tokens = field::Empty,
            codex.turn.token_usage.output_tokens = field::Empty,
            codex.turn.token_usage.reasoning_output_tokens = field::Empty,
            codex.turn.token_usage.total_tokens = field::Empty,
        );
        let handle = tokio::spawn(
            async move {
                if ready_tx.send(()).is_err() {
                    task_done_clone.notify_waiters();
                    return;
                }
                let should_run = select! {
                    start = start_rx => start.is_ok(),
                    _ = task_cancellation_token.cancelled() => false,
                };
                if !should_run {
                    task_done_clone.notify_waiters();
                    return;
                }
                let ctx_for_finish = Arc::clone(&ctx);
                let task_result = task_for_run
                    .run(
                        Arc::clone(&session),
                        ctx,
                        task_input,
                        task_cancellation_token.child_token(),
                    )
                    .instrument(trace_span!("session_task.run"))
                    .await;
                let sess = session_ctx.clone_session();
                if task_cancellation_token.is_cancelled() {
                    if let Err(err) = sess.flush_rollout().await {
                        warn!("failed to flush rollout before aborting turn: {err}");
                        sess.send_event(
                            ctx_for_finish.as_ref(),
                            EventMsg::Warning(WarningEvent {
                                message: format!(
                                    "Failed to save the conversation transcript; Codex will continue retrying. Error: {err}"
                                ),
                            }),
                        )
                        .await;
                    }
                } else {
                    // Finish uniformly from the spawn site so all tasks share the same lifecycle.
                    sess.on_task_finished(Arc::clone(&ctx_for_finish), task_result)
                        .await;
                }
                task_done_clone.notify_waiters();
            }
            .instrument(task_span),
        );
        let timer = turn_context
            .session_telemetry
            .start_timer(TURN_E2E_DURATION_METRIC, &[])
            .ok();
        let running_task = RunningTask {
            task_done,
            handle: AbortOnDropHandle::new(handle),
            kind: task_kind,
            steer_admission: SteerAdmission::Starting,
            task,
            input_persisted,
            cancellation_token,
            turn_context: Arc::clone(&turn_context),
            _agent_execution_guard: agent_execution_guard,
            _diagnostics_guard: ACTIVE_TURNS.track(),
            _timer: timer,
        };
        let install_result = {
            let mut slot = self.active_turn.lock().await;
            slot.install_running(&claim, running_task)
        };
        if let Err(err) = install_result {
            self.cancel_claimed_start(&claim).await;
            return Err(turn_slot_codex_error(err));
        }
        self.emit_turn_start_lifecycle(turn_context.as_ref(), &token_usage_at_turn_start)
            .await;
        if ready_rx.await.is_err() {
            self.cancel_claimed_start(&claim).await;
            return Err(CodexErr::Fatal(format!(
                "turn task {} exited before its start barrier was ready",
                turn_context.sub_id
            )));
        }
        task_for_start
            .emit_turn_started(session_ctx_for_start, Arc::clone(&turn_context))
            .await;
        {
            let mut slot = self.active_turn.lock().await;
            if let Err(err) = slot.open_running(&claim) {
                if let Err(cancel_err) = slot.cancel_start(&claim) {
                    warn!(%cancel_err, "failed to roll back unopened turn startup");
                }
                return Err(turn_slot_codex_error(err));
            }
            if start_tx.send(()).is_err() {
                if let Err(cancel_err) = slot.cancel_start(&claim) {
                    warn!(%cancel_err, "failed to roll back abandoned turn startup");
                }
                return Err(CodexErr::Fatal(format!(
                    "turn task {} exited before its start barrier opened",
                    turn_context.sub_id
                )));
            }
        }
        Ok(())
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the final pending-input decision and steer-admission seal must be atomic"
    )]
    pub(crate) async fn seal_regular_task_if_no_pending_input(
        &self,
        turn_id: &str,
    ) -> CodexResult<RegularTaskContinuation> {
        let mut slot = self.active_turn.lock().await;
        let Some(turn_state) = slot.turn_state().cloned() else {
            return Err(CodexErr::TurnAborted);
        };
        let Some(task) = slot.running_task_mut() else {
            return Err(CodexErr::TurnAborted);
        };
        if task.turn_context.sub_id != turn_id {
            return Err(CodexErr::TurnAborted);
        }
        if task.kind != TaskKind::Regular {
            return Err(CodexErr::Fatal(
                "only a regular task can seal steer admission after its final input check"
                    .to_string(),
            ));
        }

        let (has_turn_pending_input, accepts_mailbox_delivery) = {
            let turn_state = turn_state.lock().await;
            (
                !turn_state.pending_input.is_empty(),
                turn_state.accepts_mailbox_delivery_for_current_turn(),
            )
        };
        if accepts_mailbox_delivery
            && (has_turn_pending_input || self.input_queue.has_pending_mailbox_items().await)
        {
            return Ok(RegularTaskContinuation::Continue);
        }

        task.steer_admission = SteerAdmission::Sealed;
        Ok(RegularTaskContinuation::Sealed)
    }

    /// Returns whether an extension has marked this thread as durably asleep.
    pub(crate) fn has_outstanding_durable_sleep(&self) -> bool {
        self.services
            .thread_extension_data
            .get::<codex_extension_items::sleep::SleepItem>()
            .is_some()
    }

    /// Starts a regular turn when the session is idle and pending work is waiting.
    ///
    /// Pending work includes mailbox mail marked with `trigger_turn`, or any mailbox mail while
    /// an outstanding durable sleep is attached to the thread.
    ///
    /// This helper generates a fresh sub-id for the synthetic turn before delegating to the
    /// explicit-sub-id variant.
    pub(crate) fn maybe_start_turn_for_pending_work(self: &Arc<Self>) -> BoxFuture<'static, ()> {
        let session = Arc::clone(self);
        Box::pin(async move {
            session
                .maybe_start_turn_for_pending_work_with_sub_id(uuid::Uuid::new_v4().to_string())
                .await;
        })
    }

    /// Starts a regular turn with the provided sub-id when pending work should wake an idle
    /// session.
    ///
    /// The turn is created only when the session is idle and mailbox mail either requests a turn
    /// or can wake an outstanding durable sleep.
    pub(crate) async fn maybe_start_turn_for_pending_work_with_sub_id(
        self: &Arc<Self>,
        sub_id: String,
    ) {
        if !self.input_queue.has_pending_mailbox_items().await
            || (!self.input_queue.has_trigger_turn_mailbox_items().await
                && !self.has_outstanding_durable_sleep())
        {
            return;
        }

        let claim = {
            let mut slot = self.active_turn.lock().await;
            if !slot.is_idle() {
                return;
            }
            match slot.claim_start(sub_id.clone()) {
                Ok(claim) => claim,
                Err(err) => {
                    warn!(%err, "failed to claim idle slot for pending work");
                    return;
                }
            }
        };
        let session = Arc::clone(self);
        let startup = tokio::spawn(
            async move {
                let turn_context = session.new_default_turn_with_sub_id(sub_id).await;
                session
                    .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
                    .await;
                session
                    .start_claimed_regular_task_with_options(
                        claim,
                        turn_context,
                        Vec::new(),
                        None,
                        MailboxParentProvenance::Attribute,
                    )
                    .await
            }
            .instrument(Span::current()),
        );
        match startup.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(%err, "failed to start pending-work turn"),
            Err(err) => warn!(%err, "pending-work startup task failed"),
        }
    }

    pub async fn abort_all_tasks(self: &Arc<Self>, reason: TurnAbortReason) {
        let session = Arc::clone(self);
        let abort = tokio::spawn(
            async move { session.abort_active_turn_owned(reason).await }
                .instrument(Span::current()),
        );
        if let Err(err) = abort.await {
            warn!(%err, "turn-slot abort task failed");
        }
    }

    pub(crate) async fn abort_turn_if_active(
        self: &Arc<Self>,
        turn_id: &str,
        reason: TurnAbortReason,
    ) -> bool {
        let session = Arc::clone(self);
        let turn_id = turn_id.to_string();
        let abort = tokio::spawn(
            async move { session.abort_matching_turn_owned(&turn_id, reason).await }
                .instrument(Span::current()),
        );
        match abort.await {
            Ok(aborted) => aborted,
            Err(err) => {
                warn!(%err, "targeted turn-slot abort task failed");
                false
            }
        }
    }

    async fn abort_active_turn_owned(self: &Arc<Self>, reason: TurnAbortReason) {
        loop {
            let action = {
                let mut slot = self.active_turn.lock().await;
                if slot.is_idle() {
                    AbortSlotAction::Noop
                } else if slot.is_starting_or_transitioning()
                    || slot
                        .running_task()
                        .is_some_and(|task| task.steer_admission == SteerAdmission::Starting)
                {
                    AbortSlotAction::Wait(slot.subscribe_generation())
                } else {
                    match slot.begin_transition(terminal_transition_kind(&reason), None) {
                        Ok(retired_turn) => AbortSlotAction::Retire(retired_turn),
                        Err(err) => {
                            warn!(%err, "failed to begin turn abort transition");
                            return;
                        }
                    }
                }
            };
            let retired_turn = match action {
                AbortSlotAction::Noop => return,
                AbortSlotAction::Wait(mut generation_rx) => {
                    if generation_rx.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
                AbortSlotAction::Retire(retired_turn) => retired_turn,
            };
            let transition_generation = retired_turn.transition_generation;
            let retired_turn_id = retired_turn.task.turn_context.sub_id.clone();
            self.abort_retired_turn(retired_turn, reason.clone()).await;
            if let Err(err) = self
                .finish_transition_idle(
                    transition_generation,
                    &retired_turn_id,
                    ThreadIdleCause::Interrupted,
                )
                .await
            {
                warn!(%err, "failed to finish turn abort transition");
                return;
            }
            if reason == TurnAbortReason::Interrupted {
                self.maybe_start_turn_for_pending_work().await;
            }
            return;
        }
    }

    async fn abort_matching_turn_owned(
        self: &Arc<Self>,
        turn_id: &str,
        reason: TurnAbortReason,
    ) -> bool {
        loop {
            let action = {
                let mut slot = self.active_turn.lock().await;
                if slot
                    .running_turn_id()
                    .is_some_and(|active_id| active_id != turn_id)
                    || slot.is_idle()
                    || slot.is_starting_or_transitioning()
                {
                    AbortSlotAction::Noop
                } else if slot
                    .running_task()
                    .is_some_and(|task| task.steer_admission == SteerAdmission::Starting)
                {
                    AbortSlotAction::Wait(slot.subscribe_generation())
                } else {
                    match slot.begin_transition(terminal_transition_kind(&reason), None) {
                        Ok(retired_turn) => AbortSlotAction::Retire(retired_turn),
                        Err(err) => {
                            warn!(%err, "failed to begin targeted turn abort transition");
                            return false;
                        }
                    }
                }
            };
            let retired_turn = match action {
                AbortSlotAction::Noop => return false,
                AbortSlotAction::Wait(mut generation_rx) => {
                    if generation_rx.changed().await.is_err() {
                        return false;
                    }
                    continue;
                }
                AbortSlotAction::Retire(retired_turn) => retired_turn,
            };
            let transition_generation = retired_turn.transition_generation;
            let retired_turn_id = retired_turn.task.turn_context.sub_id.clone();
            self.abort_retired_turn(retired_turn, reason.clone()).await;
            if let Err(err) = self
                .finish_transition_idle(
                    transition_generation,
                    &retired_turn_id,
                    ThreadIdleCause::Interrupted,
                )
                .await
            {
                warn!(%err, "failed to finish targeted turn abort transition");
                return false;
            }
            if reason == TurnAbortReason::Interrupted {
                self.maybe_start_turn_for_pending_work().await;
            }
            return true;
        }
    }

    async fn abort_retired_turn(
        self: &Arc<Self>,
        retired_turn: RetiredTurn,
        reason: TurnAbortReason,
    ) {
        let turn_context = Arc::clone(&retired_turn.task.turn_context);
        self.handle_task_abort(retired_turn.task, reason.clone())
            .await;
        self.emit_turn_abort_lifecycle(reason, turn_context.extension_data.as_ref())
            .await;
        // Let interrupted tasks observe cancellation before dropping pending approvals, or an
        // in-flight approval wait can surface as a model-visible rejection before TurnAborted.
        self.input_queue
            .clear_pending_for_turn_state(retired_turn.turn_state.as_ref())
            .await;
    }

    async fn finish_transition_idle(
        self: &Arc<Self>,
        transition_generation: u64,
        retired_turn_id: &str,
        cause: ThreadIdleCause,
    ) -> Result<(), TurnSlotError> {
        {
            let mut slot = self.active_turn.lock().await;
            slot.finish_transition_idle(transition_generation, retired_turn_id)?;
        }
        self.emit_thread_idle_lifecycle_if_idle(cause).await;
        Ok(())
    }

    pub async fn on_task_finished(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        task_result: SessionTaskResult,
    ) {
        let (mut task_output, abort_reason) = match task_result {
            Ok(task_output) => (task_output, None),
            Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => (
                SessionTaskOutput::default(),
                Some(TurnAbortReason::Interrupted),
            ),
            Err(err) => {
                warn!(%err, "session task returned an unexpected error");
                self.emit_turn_error_lifecycle(
                    turn_context.as_ref(),
                    err.to_codex_protocol_error(),
                )
                .await;
                self.track_turn_codex_error(turn_context.as_ref(), &err);
                self.send_event(
                    turn_context.as_ref(),
                    EventMsg::Error(err.to_error_event(/*message_prefix*/ None)),
                )
                .await;
                (SessionTaskOutput::default(), None)
            }
        };
        turn_context
            .turn_metadata_state
            .cancel_git_enrichment_task();

        let transition_kind = if abort_reason.is_some() {
            TerminalTransitionKind::Interrupting
        } else {
            TerminalTransitionKind::Completing
        };
        let retired_turn = {
            let mut slot = self.active_turn.lock().await;
            if slot.running_turn_id() != Some(turn_context.sub_id.as_str()) {
                return;
            }
            match slot.begin_transition(transition_kind, None) {
                Ok(retired_turn) => retired_turn,
                Err(err) => {
                    warn!(%err, "failed to begin task completion transition");
                    return;
                }
            }
        };
        let RetiredTurn {
            transition_generation,
            task,
            turn_state,
        } = retired_turn;
        let mut task = task;
        let mut task_ended_before_persistence = if let Some(sender) = task.input_persisted.take() {
            let _ = sender.send(Err(
                TryStartTurnIfIdleRejectionReason::TaskEndedBeforePersistence,
            ));
            true
        } else {
            false
        };
        let steer_admission = task.steer_admission;
        task.handle.detach();

        if let Err(err) = self.flush_rollout().await {
            warn!("failed to flush rollout before completing turn: {err}");
            self.send_event(
                turn_context.as_ref(),
                EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Failed to save the conversation transcript; Codex will continue retrying. Error: {err}"
                    ),
                }),
            )
            .await;
        }

        let mut recovery_application_error = None;
        if let Some(recovery) = task_output.post_compact_recovery.take() {
            let result = if steer_admission == SteerAdmission::Sealed {
                self.record_post_compact_recovery_sampling_success(&recovery, &turn_context.sub_id)
                    .await
            } else {
                Err(CodexErr::Fatal(
                    "post-compact recovery reached task completion before steer admission was sealed"
                        .to_string(),
                ))
            };
            if let Err(err) = result {
                warn!(%err, "failed to record post-compact recovery application");
                task_output.last_agent_message = None;
                recovery_application_error = Some(err);
            }
        }
        if let Some(error) = recovery_application_error.as_ref() {
            self.emit_turn_error_lifecycle(turn_context.as_ref(), error.to_codex_protocol_error())
                .await;
            self.track_turn_codex_error(turn_context.as_ref(), error);
            self.send_event(
                turn_context.as_ref(),
                EventMsg::Error(error.to_error_event(/*message_prefix*/ None)),
            )
            .await;
        }
        let last_agent_message = task_output.last_agent_message;
        let pending_input = self
            .input_queue
            .take_pending_input_for_turn_state(turn_state.as_ref())
            .await;
        let (turn_had_memory_citation, turn_tool_calls, token_usage_at_turn_start) = {
            let ts = turn_state.lock().await;
            (
                ts.has_memory_citation,
                ts.tool_calls,
                ts.token_usage_at_turn_start.clone(),
            )
        };
        run_hooks_and_record_inputs(self, &turn_context, &pending_input).await;
        task_ended_before_persistence |= self
            .pending_user_message_admissions
            .complete_task_end(&turn_context.sub_id);
        // Emit token usage metrics.
        {
            // TODO(jif): drop this
            let tmp_mem = (
                "tmp_mem_enabled",
                if self.enabled(Feature::MemoryTool) {
                    "true"
                } else {
                    "false"
                },
            );
            let network_proxy = self.services.network_proxy.load_full();
            let network_proxy_active = match network_proxy.as_ref() {
                Some(started_network_proxy) => {
                    match started_network_proxy.proxy().current_cfg().await {
                        Ok(config) => config.enabled,
                        Err(err) => {
                            warn!(
                                "failed to read managed network proxy state for turn metrics: {err:#}"
                            );
                            false
                        }
                    }
                }
                None => false,
            };
            emit_turn_network_proxy_metric(
                &self.services.session_telemetry,
                network_proxy_active,
                tmp_mem,
            );
            self.services.session_telemetry.histogram(
                TURN_TOOL_CALL_METRIC,
                i64::try_from(turn_tool_calls).unwrap_or(i64::MAX),
                &[tmp_mem],
            );
            let total_token_usage = self.total_token_usage().await.unwrap_or_default();
            let turn_token_usage = TokenUsage {
                input_tokens: (total_token_usage.input_tokens
                    - token_usage_at_turn_start.input_tokens)
                    .max(0),
                cached_input_tokens: (total_token_usage.cached_input_tokens
                    - token_usage_at_turn_start.cached_input_tokens)
                    .max(0),
                cache_write_input_tokens: (total_token_usage.cache_write_input_tokens
                    - token_usage_at_turn_start.cache_write_input_tokens)
                    .max(0),
                output_tokens: (total_token_usage.output_tokens
                    - token_usage_at_turn_start.output_tokens)
                    .max(0),
                reasoning_output_tokens: (total_token_usage.reasoning_output_tokens
                    - token_usage_at_turn_start.reasoning_output_tokens)
                    .max(0),
                total_tokens: (total_token_usage.total_tokens
                    - token_usage_at_turn_start.total_tokens)
                    .max(0),
                codex_rollout_budget_units: None,
            };
            let current_span = Span::current();
            current_span.record(
                "codex.turn.token_usage.input_tokens",
                turn_token_usage.input_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.cached_input_tokens",
                turn_token_usage.cached_input(),
            );
            current_span.record(
                "codex.turn.token_usage.cache_write_input_tokens",
                turn_token_usage.cache_write_input_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.non_cached_input_tokens",
                turn_token_usage.non_cached_input(),
            );
            current_span.record(
                "codex.turn.token_usage.output_tokens",
                turn_token_usage.output_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.reasoning_output_tokens",
                turn_token_usage.reasoning_output_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.total_tokens",
                turn_token_usage.total_tokens,
            );
            self.services
                .analytics_events_client
                .track_turn_token_usage(TurnTokenUsageFact {
                    turn_id: turn_context.sub_id.clone(),
                    thread_id: self.thread_id.to_string(),
                    token_usage: turn_token_usage.clone(),
                });
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.total_tokens,
                &[("token_type", "total"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.input_tokens,
                &[("token_type", "input"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.cached_input(),
                &[("token_type", "cached_input"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.cache_write_input_tokens,
                &[("token_type", "cache_write_input"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.output_tokens,
                &[("token_type", "output"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.reasoning_output_tokens,
                &[("token_type", "reasoning_output"), tmp_mem],
            );
        }
        emit_turn_memory_metric(
            &self.services.session_telemetry,
            turn_context.config.features.enabled(Feature::MemoryTool),
            turn_context.config.memories.use_memories,
            turn_had_memory_citation,
        );
        self.services.session_telemetry.counter(
            TURN_UNIFIED_EXEC_RUNNING_PROCESSES_METRIC,
            i64::try_from(self.list_background_terminals().await.len()).unwrap_or(i64::MAX),
            &[],
        );
        let started_at = turn_context.turn_timing_state.started_at_unix_secs().await;
        let (completed_at, duration_ms, profile) = turn_context
            .turn_timing_state
            .complete_profile_and_duration_ms()
            .await;
        self.services
            .analytics_events_client
            .track_turn_profile(TurnProfileFact {
                turn_id: turn_context.sub_id.clone(),
                profile,
            });
        let idle_cause = if matches!(abort_reason.as_ref(), Some(TurnAbortReason::Interrupted)) {
            ThreadIdleCause::Interrupted
        } else if task_ended_before_persistence
            || (abort_reason.is_none() && turn_context.terminal_error.lock().await.is_some())
        {
            ThreadIdleCause::Failed
        } else {
            ThreadIdleCause::Completed
        };
        let event = if let Some(reason) = abort_reason {
            self.emit_turn_abort_lifecycle(reason.clone(), turn_context.extension_data.as_ref())
                .await;
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: Some(turn_context.sub_id.clone()),
                reason,
                started_at,
                completed_at,
                duration_ms,
            })
        } else {
            let time_to_first_token_ms = turn_context
                .turn_timing_state
                .time_to_first_token_ms()
                .await;
            let error = turn_context.terminal_error.lock().await.clone();
            self.emit_turn_stop_lifecycle(turn_context.extension_data.as_ref())
                .await;
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: turn_context.sub_id.clone(),
                last_agent_message,
                error,
                started_at,
                completed_at,
                duration_ms,
                time_to_first_token_ms,
            })
        };
        self.send_event(turn_context.as_ref(), event).await;
        self.services
            .guardian_rejection_circuit_breaker
            .lock()
            .await
            .clear_turn(&turn_context.sub_id);

        // Regular items were flushed before this terminal event was appended; buffering
        // thread writers may not flush it without another explicit barrier.
        if let Err(err) = self.flush_rollout().await {
            warn!("failed to flush rollout after emitting terminal turn event: {err}");
        }
        if let Err(err) = self
            .finish_transition_idle(transition_generation, &turn_context.sub_id, idle_cause)
            .await
        {
            warn!(%err, "failed to finish task completion transition");
            return;
        }
        self.maybe_start_turn_for_pending_work().await;
    }

    pub(crate) async fn close_unified_exec_processes(&self) {
        self.services
            .unified_exec_manager
            .terminate_all_processes()
            .await;
    }

    pub(crate) async fn list_background_terminals(&self) -> Vec<BackgroundTerminalInfo> {
        self.services.unified_exec_manager.list_processes().await
    }

    pub(crate) async fn terminate_background_terminal(&self, process_id: i32) -> bool {
        self.services
            .unified_exec_manager
            .terminate_process(process_id)
            .await
    }

    async fn handle_task_abort(self: &Arc<Self>, mut task: RunningTask, reason: TurnAbortReason) {
        let sub_id = task.turn_context.sub_id.clone();
        if task.cancellation_token.is_cancelled() {
            return;
        }

        if let Some(sender) = task.input_persisted.take() {
            let _ = sender.send(Err(
                TryStartTurnIfIdleRejectionReason::TaskEndedBeforePersistence,
            ));
        }
        self.pending_user_message_admissions
            .complete_task_end(&sub_id);
        trace!(task_kind = ?task.kind, sub_id, "aborting running task");
        task.cancellation_token.cancel();
        if reason == TurnAbortReason::Interrupted
            && task
                .turn_context
                .config
                .features
                .enabled(Feature::CodeModeInterrupt)
        {
            self.services
                .code_mode_service
                .interrupt_active_cells()
                .await;
        }
        task.turn_context
            .turn_metadata_state
            .cancel_git_enrichment_task();
        let session_task = task.task;

        select! {
            _ = task.task_done.notified() => {
            },
            _ = tokio::time::sleep(Duration::from_millis(GRACEFULL_INTERRUPTION_TIMEOUT_MS)) => {
                warn!("task {sub_id} didn't complete gracefully after {}ms", GRACEFULL_INTERRUPTION_TIMEOUT_MS);
            }
        }

        task.handle.abort();

        session_task
            .abort(Arc::clone(self), Arc::clone(&task.turn_context))
            .await;

        if reason == TurnAbortReason::Interrupted
            && let Some(marker) = interrupted_turn_history_marker(
                InterruptedTurnHistoryMarker::from_config_and_version(
                    task.turn_context.config.as_ref(),
                    task.turn_context.multi_agent_version,
                ),
            )
        {
            self.record_conversation_items(
                task.turn_context.as_ref(),
                std::slice::from_ref(&marker),
            )
            .await;
            // Ensure the marker is durably visible before emitting TurnAborted: some clients
            // synchronously re-read the rollout on receipt of the abort event.
            if let Err(err) = self.flush_rollout().await {
                warn!("failed to flush interrupted-turn marker before emitting TurnAborted: {err}");
            }
        }

        let started_at = task
            .turn_context
            .turn_timing_state
            .started_at_unix_secs()
            .await;
        let (completed_at, duration_ms, profile) = task
            .turn_context
            .turn_timing_state
            .complete_profile_and_duration_ms()
            .await;
        self.services
            .analytics_events_client
            .track_turn_profile(TurnProfileFact {
                turn_id: task.turn_context.sub_id.clone(),
                profile,
            });
        let event = EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some(task.turn_context.sub_id.clone()),
            reason,
            started_at,
            completed_at,
            duration_ms,
        });
        self.send_event(task.turn_context.as_ref(), event).await;
        self.services
            .guardian_rejection_circuit_breaker
            .lock()
            .await
            .clear_turn(&task.turn_context.sub_id);
        // Regular items were flushed before this terminal event was appended; buffering
        // thread writers may not flush it without another explicit barrier.
        if let Err(err) = self.flush_rollout().await {
            warn!("failed to flush rollout after emitting terminal turn event: {err}");
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
