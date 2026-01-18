use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;

use crate::client_common::SANITIZE_PROMPT;
use crate::codex::TurnContext;
use crate::codex::run_sampling_request;
use crate::error::CodexErr;
use crate::protocol::EventMsg;
use crate::protocol::TurnStartedEvent;
use crate::state::TaskKind;
use crate::tools::context::SharedTurnDiffTracker;
use crate::turn_diff_tracker::TurnDiffTracker;

use super::SessionTask;
use super::SessionTaskContext;

const MAX_MANAGE_CONTEXT_CALLS_IN_PROMPT: usize = 10;

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

        let started = EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: ctx.client.get_model_context_window(),
        });
        sess.send_event(ctx.as_ref(), started).await;

        let sanitize_prompt = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: SANITIZE_PROMPT.to_string(),
            }],
        };

        let turn_diff_tracker: SharedTurnDiffTracker =
            Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let mut client_session = ctx.client.new_session();

        loop {
            if cancellation_token.is_cancelled() {
                return None;
            }

            let manage_context_items = {
                let state = sess.state_lock().await;
                let (items, _) = state.history.items_with_rids();
                collect_recent_manage_context_items(items, MAX_MANAGE_CONTEXT_CALLS_IN_PROMPT)
            };

            let mut input = Vec::with_capacity(1 + manage_context_items.len());
            input.push(sanitize_prompt.clone());
            input.extend(manage_context_items);

            let out = match run_sampling_request(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                Arc::clone(&turn_diff_tracker),
                &mut client_session,
                input,
                cancellation_token.child_token(),
            )
            .await
            {
                Ok(out) => out,
                Err(CodexErr::TurnAborted | CodexErr::Interrupted) => return None,
                Err(CodexErr::ContextWindowExceeded) => {
                    sess.set_total_tokens_full(ctx.as_ref()).await;
                    return None;
                }
                Err(e) => {
                    sess.send_event(ctx.as_ref(), EventMsg::Error(e.to_error_event(None)))
                        .await;
                    return None;
                }
            };

            if !out.needs_follow_up {
                return out.last_agent_message;
            }
        }
    }
}

fn collect_recent_manage_context_items(
    items: &[ResponseItem],
    max_calls: usize,
) -> Vec<ResponseItem> {
    if max_calls == 0 {
        return Vec::new();
    }

    let mut call_ids = HashSet::new();
    for item in items.iter().rev() {
        match item {
            ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                call_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                call_ids.insert(call_id.clone());
            }
            _ => {}
        }
        if call_ids.len() >= max_calls {
            break;
        }
    }

    if call_ids.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for item in items {
        match item {
            ResponseItem::FunctionCall { name, call_id, .. }
                if name == "manage_context" && call_ids.contains(call_id) =>
            {
                out.push(item.clone());
            }
            ResponseItem::CustomToolCall { name, call_id, .. }
                if name == "manage_context" && call_ids.contains(call_id) =>
            {
                out.push(item.clone());
            }
            ResponseItem::FunctionCallOutput { call_id, .. } if call_ids.contains(call_id) => {
                out.push(item.clone());
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } if call_ids.contains(call_id) => {
                out.push(item.clone());
            }
            _ => {}
        }
    }
    out
}
