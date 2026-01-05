use std::sync::Arc;

use async_trait::async_trait;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;

use crate::codex::TurnContext;
use crate::codex::run_turn;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::protocol::EventMsg;
use crate::protocol::TaskStartedEvent;
use crate::state::TaskKind;
use crate::tools::context::SharedTurnDiffTracker;
use crate::turn_diff_tracker::TurnDiffTracker;

use super::SessionTask;
use super::SessionTaskContext;

#[derive(Clone, Copy)]
pub(crate) struct SanitizeTask;

impl SanitizeTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionTask for SanitizeTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Sanitize
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let sess = session.clone_session();

        let started = EventMsg::TaskStarted(TaskStartedEvent {
            model_context_window: ctx.client.get_model_context_window(),
        });
        sess.send_event(&ctx, started).await;

        let mut thread_items = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: crate::client_common::SANITIZE_PROMPT.to_string(),
            }],
        }];

        let mut history_cursor = sess.state_lock().await.history_snapshot_lenient().len();
        let turn_diff_tracker: SharedTurnDiffTracker =
            Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

        loop {
            if cancellation_token.is_cancelled() {
                return None;
            }

            match run_sanitize_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                Arc::clone(&turn_diff_tracker),
                thread_items.clone(),
                cancellation_token.child_token(),
            )
            .await
            {
                Ok(result) => {
                    if !result.needs_follow_up {
                        return result.last_agent_message;
                    }
                }
                Err(CodexErr::TurnAborted | CodexErr::Interrupted) => return None,
                Err(CodexErr::ContextWindowExceeded) => {
                    sess.set_total_tokens_full(&ctx).await;
                    return None;
                }
                Err(e) => {
                    sess.send_event(&ctx, EventMsg::Error(e.to_error_event(None)))
                        .await;
                    return None;
                }
            }

            let before_cursor = history_cursor;
            let history = sess.state_lock().await.history_snapshot_lenient();
            let new_items = history.get(history_cursor..).unwrap_or_default();
            history_cursor = history.len();
            thread_items.extend(new_items.iter().cloned());

            if history_cursor == before_cursor {
                sess.send_event(
                    &ctx,
                    EventMsg::Error(crate::protocol::ErrorEvent {
                        message:
                            "Sanitize is stuck (no new items recorded between follow-up turns)."
                                .to_string(),
                        codex_error_info: None,
                    }),
                )
                .await;
                return None;
            }
        }
    }
}

struct SanitizeTurnResult {
    needs_follow_up: bool,
    last_agent_message: Option<String>,
}

async fn run_sanitize_turn(
    sess: Arc<crate::codex::Session>,
    ctx: Arc<TurnContext>,
    turn_diff_tracker: SharedTurnDiffTracker,
    input: Vec<ResponseItem>,
    cancellation_token: CancellationToken,
) -> CodexResult<SanitizeTurnResult> {
    let out = run_turn(sess, ctx, turn_diff_tracker, input, cancellation_token).await?;
    Ok(SanitizeTurnResult {
        needs_follow_up: out.needs_follow_up,
        last_agent_message: out.last_agent_message,
    })
}
