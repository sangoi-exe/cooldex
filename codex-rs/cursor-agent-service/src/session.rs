use crate::AgentServiceRun;
use crate::AgentServiceTransportError;
use crate::CursorMappingError;
use crate::CursorToolCallTracker;
use crate::CursorToolSnapshot;
use crate::map_interaction_update;
use crate::map_request_context_result;
use crate::proto::AgentServerMessage;
use crate::proto::agent_server_message;
use crate::proto::exec_server_message;
use codex_api::ResponseEvent;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const EVENT_CAPACITY: usize = 64;

#[derive(Debug, Error)]
pub enum CursorAgentServiceSessionError {
    #[error(transparent)]
    Transport(#[from] AgentServiceTransportError),
    #[error(transparent)]
    Mapping(#[from] CursorMappingError),
    #[error("Cursor AgentService tool-result channel is closed")]
    ToolResultChannelClosed,
    #[error("Cursor AgentService transport admitted an unsupported server message")]
    ValidatedServerMessageMismatch,
}

/// One ephemeral Cursor AgentService Run projected into Cooldex response events.
pub struct CursorSamplingSession {
    events: Option<mpsc::Receiver<Result<ResponseEvent, CursorAgentServiceSessionError>>>,
    tool_results: Option<mpsc::Sender<ResponseInputItem>>,
    consumer_dropped: CancellationToken,
    cancel_on_drop: bool,
}

impl CursorSamplingSession {
    pub fn start(
        run: AgentServiceRun,
        tool_snapshot: CursorToolSnapshot,
        base_instructions: String,
        response_id: String,
        max_pending_tool_actions: usize,
        consumer_dropped: CancellationToken,
    ) -> Self {
        let (event_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let (tool_result_tx, tool_results) = mpsc::channel(max_pending_tool_actions);
        let worker_cancel = consumer_dropped.clone();
        tokio::spawn(async move {
            let result = run_session(
                run,
                tool_snapshot,
                base_instructions,
                response_id,
                max_pending_tool_actions,
                worker_cancel,
                &event_tx,
                tool_results,
            )
            .await;
            if let Err(error) = result {
                let _ = event_tx.send(Err(error)).await;
            }
        });

        Self {
            events: Some(events),
            tool_results: Some(tool_result_tx),
            consumer_dropped,
            cancel_on_drop: true,
        }
    }

    pub async fn next_event(
        &mut self,
    ) -> Option<Result<ResponseEvent, CursorAgentServiceSessionError>> {
        self.events
            .as_mut()
            .expect("Cursor sampling session events are already detached")
            .recv()
            .await
    }

    pub async fn send_tool_result(
        &self,
        result: ResponseInputItem,
    ) -> Result<(), CursorAgentServiceSessionError> {
        self.tool_results
            .as_ref()
            .expect("Cursor sampling session tool results are already detached")
            .send(result)
            .await
            .map_err(|_| CursorAgentServiceSessionError::ToolResultChannelClosed)
    }

    pub fn into_parts(
        mut self,
    ) -> (
        mpsc::Receiver<Result<ResponseEvent, CursorAgentServiceSessionError>>,
        mpsc::Sender<ResponseInputItem>,
        CancellationToken,
    ) {
        self.cancel_on_drop = false;
        (
            self.events
                .take()
                .expect("Cursor sampling session events are already detached"),
            self.tool_results
                .take()
                .expect("Cursor sampling session tool results are already detached"),
            self.consumer_dropped.clone(),
        )
    }
}

impl Drop for CursorSamplingSession {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.consumer_dropped.cancel();
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    mut run: AgentServiceRun,
    tool_snapshot: CursorToolSnapshot,
    base_instructions: String,
    response_id: String,
    max_pending_tool_actions: usize,
    consumer_dropped: CancellationToken,
    events: &mpsc::Sender<Result<ResponseEvent, CursorAgentServiceSessionError>>,
    mut tool_results: mpsc::Receiver<ResponseInputItem>,
) -> Result<(), CursorAgentServiceSessionError> {
    if !send_event(events, ResponseEvent::Created).await {
        return Ok(());
    }

    let mut tracker = CursorToolCallTracker::new(tool_snapshot, max_pending_tool_actions);
    let mut active_assistant_message: Option<ActiveAssistantMessage> = None;
    let mut tool_results_open = true;

    loop {
        tokio::select! {
            biased;
            _ = consumer_dropped.cancelled() => return Ok(()),
            result = tool_results.recv(), if tool_results_open => {
                let Some(result) = result else {
                    tool_results_open = false;
                    if tracker.pending_count() > 0 {
                        return Err(CursorAgentServiceSessionError::ToolResultChannelClosed);
                    }
                    continue;
                };
                let completed = tracker.complete_cooldex_call(&result)?;
                run.send_exec_client_message(completed.exec_client_message).await?;
            }
            server_message = run.next_server_message() => {
                let server_message = server_message?;
                let terminal = handle_server_message(
                    &run,
                    server_message,
                    &base_instructions,
                    &response_id,
                    &mut tracker,
                    &mut active_assistant_message,
                    events,
                ).await?;
                if terminal {
                    return Ok(());
                }
            }
        }
    }
}

async fn handle_server_message(
    run: &AgentServiceRun,
    message: AgentServerMessage,
    base_instructions: &str,
    response_id: &str,
    tracker: &mut CursorToolCallTracker,
    active_assistant_message: &mut Option<ActiveAssistantMessage>,
    events: &mpsc::Sender<Result<ResponseEvent, CursorAgentServiceSessionError>>,
) -> Result<bool, CursorAgentServiceSessionError> {
    match message.message {
        Some(agent_server_message::Message::InteractionUpdate(update)) => {
            match map_interaction_update(response_id, &update)? {
                Some(ResponseEvent::OutputTextDelta(delta)) => {
                    if active_assistant_message.is_none() {
                        let message = ActiveAssistantMessage::new();
                        if !send_event(events, ResponseEvent::OutputItemAdded(message.item())).await {
                            return Ok(true);
                        }
                        *active_assistant_message = Some(message);
                    }
                    active_assistant_message
                        .as_mut()
                        .expect("assistant message was initialized")
                        .text
                        .push_str(&delta);
                    if !send_event(events, ResponseEvent::OutputTextDelta(delta)).await {
                        return Ok(true);
                    }
                    Ok(false)
                }
                Some(completed @ ResponseEvent::Completed { .. }) => {
                    tracker.require_no_pending()?;
                    if !flush_assistant_message(active_assistant_message, events).await {
                        return Ok(true);
                    }
                    let _ = send_event(events, completed).await;
                    Ok(true)
                }
                None => Ok(false),
                Some(_) => Err(CursorAgentServiceSessionError::ValidatedServerMessageMismatch),
            }
        }
        Some(agent_server_message::Message::ExecServerMessage(exec)) => {
            match exec.message {
                Some(exec_server_message::Message::RequestContextArgs(args)) => {
                    let result = map_request_context_result(
                        exec.id,
                        exec.exec_id,
                        &args,
                        base_instructions,
                    )?;
                    run.send_exec_client_message(result).await?;
                    Ok(false)
                }
                Some(exec_server_message::Message::McpArgs(args)) => {
                    if !flush_assistant_message(active_assistant_message, events).await {
                        return Ok(true);
                    }
                    let cooldex_call_id = ResponseItemId::new("call").to_string();
                    let accepted = tracker.accept_mcp_call(
                        exec.id,
                        exec.exec_id,
                        args,
                        cooldex_call_id,
                    )?;
                    let mut item = accepted.response_item;
                    let prefix = item.id_prefix().unwrap_or("item");
                    item.set_id(Some(ResponseItemId::new(prefix)));
                    if !send_event(events, ResponseEvent::OutputItemAdded(item.clone())).await {
                        return Ok(true);
                    }
                    if !send_event(events, ResponseEvent::OutputItemDone(item)).await {
                        return Ok(true);
                    }
                    Ok(false)
                }
                None
                | Some(
                    exec_server_message::Message::ShellArgs(_)
                    | exec_server_message::Message::WriteArgs(_)
                    | exec_server_message::Message::DeleteArgs(_)
                    | exec_server_message::Message::GrepArgs(_)
                    | exec_server_message::Message::ReadArgs(_)
                    | exec_server_message::Message::LsArgs(_)
                    | exec_server_message::Message::DiagnosticsArgs(_)
                    | exec_server_message::Message::ShellStreamArgs(_)
                    | exec_server_message::Message::BackgroundShellSpawnArgs(_)
                    | exec_server_message::Message::ListMcpResourcesExecArgs(_)
                    | exec_server_message::Message::ReadMcpResourceExecArgs(_)
                    | exec_server_message::Message::FetchArgs(_)
                    | exec_server_message::Message::RecordScreenArgs(_)
                    | exec_server_message::Message::ComputerUseArgs(_)
                    | exec_server_message::Message::WriteShellStdinArgs(_)
                    | exec_server_message::Message::ExecuteHookArgs(_)
                    | exec_server_message::Message::SubagentArgs(_)
                    | exec_server_message::Message::RedactedReadArgs(_)
                    | exec_server_message::Message::ForceBackgroundShellArgs(_)
                    | exec_server_message::Message::ForceBackgroundSubagentArgs(_)
                    | exec_server_message::Message::McpStateExecArgs(_)
                    | exec_server_message::Message::SubagentAwaitArgs(_)
                    | exec_server_message::Message::SmartModeClassifierArgs(_)
                    | exec_server_message::Message::CanvasDiagnosticsArgs(_)
                    | exec_server_message::Message::ShellAllowlistPrecheckArgs(_)
                    | exec_server_message::Message::McpAllowlistPrecheckArgs(_)
                    | exec_server_message::Message::WebFetchAllowlistPrecheckArgs(_)
                    | exec_server_message::Message::GitDiffRequest(_)
                    | exec_server_message::Message::PiReadArgs(_)
                    | exec_server_message::Message::PiBashArgs(_)
                    | exec_server_message::Message::PiEditArgs(_)
                    | exec_server_message::Message::PiWriteArgs(_)
                    | exec_server_message::Message::PiGrepArgs(_)
                    | exec_server_message::Message::PiFindArgs(_)
                    | exec_server_message::Message::PiLsArgs(_)
                    | exec_server_message::Message::MiniSweAgentBashArgs(_)
                    | exec_server_message::Message::ConversationSearchArgs(_)
                    | exec_server_message::Message::AgentStoreConflictArgs(_),
                ) => Err(CursorAgentServiceSessionError::ValidatedServerMessageMismatch),
            }
        }
        None
        | Some(
            agent_server_message::Message::ConversationCheckpointUpdate(_)
            | agent_server_message::Message::KvServerMessage(_)
            | agent_server_message::Message::ExecServerControlMessage(_)
            | agent_server_message::Message::InteractionQuery(_),
        ) => Err(CursorAgentServiceSessionError::ValidatedServerMessageMismatch),
    }
}

async fn send_event(
    events: &mpsc::Sender<Result<ResponseEvent, CursorAgentServiceSessionError>>,
    event: ResponseEvent,
) -> bool {
    events.send(Ok(event)).await.is_ok()
}

async fn flush_assistant_message(
    active: &mut Option<ActiveAssistantMessage>,
    events: &mpsc::Sender<Result<ResponseEvent, CursorAgentServiceSessionError>>,
) -> bool {
    let Some(message) = active.take() else {
        return true;
    };
    send_event(events, ResponseEvent::OutputItemDone(message.item())).await
}

struct ActiveAssistantMessage {
    id: ResponseItemId,
    text: String,
}

impl ActiveAssistantMessage {
    fn new() -> Self {
        Self {
            id: ResponseItemId::new("msg"),
            text: String::new(),
        }
    }

    fn item(&self) -> ResponseItem {
        ResponseItem::Message {
            id: Some(self.id.clone()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: self.text.clone(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }
}
