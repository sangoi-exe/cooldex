use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use tracing::Instrument;
use tracing::trace_span;

use super::RegularTaskContinuation;
use super::SessionTask;
use super::SessionTaskContext;
use super::SessionTaskOutput;
use super::SessionTaskResult;
use super::emit_standard_turn_started;

#[derive(Default)]
pub(crate) struct RegularTask;

impl RegularTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    fn emit_turn_started(
        &self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> impl std::future::Future<Output = ()> + Send {
        emit_standard_turn_started(session, ctx)
    }

    async fn run(
        self: Arc<Self>,
        sess: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let run_turn_span = trace_span!("run_turn");
        let prewarmed_client_session = async {
            sess.set_server_reasoning_included(/*included*/ false).await;
            sess.consume_startup_prewarm_for_regular_turn(&cancellation_token)
                .await
        }
        .instrument(trace_span!("regular_task.prepare_run_turn"))
        .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                run_hooks_and_record_inputs(&sess, &ctx, &input).await;
                return Ok(SessionTaskOutput::default());
            }
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let mut next_input = input;
        let mut prewarmed_client_session = prewarmed_client_session;
        loop {
            let turn_output = run_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                next_input,
                prewarmed_client_session.take(),
                cancellation_token.child_token(),
            )
            .instrument(run_turn_span.clone())
            .await?;
            match sess
                .seal_regular_task_if_no_pending_input(&ctx.sub_id)
                .await?
            {
                RegularTaskContinuation::Continue => {
                    next_input = Vec::new();
                }
                RegularTaskContinuation::Sealed => {
                    return Ok(SessionTaskOutput {
                        last_agent_message: turn_output.last_agent_message,
                        post_compact_recovery: turn_output.post_compact_recovery,
                    });
                }
            }
        }
    }
}
