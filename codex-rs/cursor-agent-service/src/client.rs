use crate::proto::AgentClientMessage;
use crate::proto::AgentRunRequest;
use crate::proto::AgentServerMessage;
use crate::proto::ClientHeartbeat;
use crate::proto::HEARTBEAT_INTERVAL_SECONDS;
use crate::proto::agent_client_message;
use crate::proto::agent_server_message;
use crate::proto::agent_service_client::AgentServiceClient;
use crate::proto::exec_server_message;
use crate::proto::interaction_update;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Request;
use tonic::Streaming;
use tonic::transport::Channel;
use tonic::transport::Endpoint;

const OUTBOUND_MESSAGE_CAPACITY: usize = 16;
const MAX_WIRE_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AgentServiceTransportError {
    #[error("invalid Cursor AgentService origin: {0}")]
    InvalidOrigin(String),
    #[error("failed to connect to Cursor AgentService: {0}")]
    Connect(String),
    #[error("Cursor AgentService Run failed: {0}")]
    Rpc(String),
    #[error("Cursor AgentService closed the client side of Run")]
    OutboundClosed,
    #[error("Cursor AgentService ended the stream before turnEnded")]
    UnexpectedEof,
    #[error("Cursor AgentService returned an empty server message")]
    EmptyServerMessage,
    #[error("Cursor AgentService returned an empty interaction update")]
    EmptyInteractionUpdate,
    #[error("Cursor AgentService returned an empty exec message")]
    EmptyExecServerMessage,
    #[error("Cursor AgentService returned an unsupported server message")]
    UnsupportedServerMessage,
    #[error("Cursor AgentService returned an unsupported interaction update")]
    UnsupportedInteractionUpdate,
    #[error("Cursor AgentService requested an unsupported internal action")]
    UnsupportedExecServerMessage,
    #[error("Cursor AgentService Run is no longer active")]
    RunClosed,
}

#[derive(Debug)]
pub struct AgentServiceTransport {
    client: AgentServiceClient<Channel>,
}

impl AgentServiceTransport {
    pub async fn connect(origin: &str) -> Result<Self, AgentServiceTransportError> {
        let endpoint = Endpoint::from_shared(origin.to_string())
            .map_err(|error| AgentServiceTransportError::InvalidOrigin(error.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .map_err(|error| AgentServiceTransportError::Connect(error.to_string()))?;
        let client = AgentServiceClient::new(channel)
            .max_decoding_message_size(MAX_WIRE_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_WIRE_MESSAGE_BYTES);
        Ok(Self { client })
    }

    pub async fn start_run(
        &mut self,
        run_request: AgentRunRequest,
    ) -> Result<AgentServiceRun, AgentServiceTransportError> {
        let (outbound, outbound_rx) = mpsc::channel(OUTBOUND_MESSAGE_CAPACITY);
        outbound
            .send(AgentClientMessage {
                message: Some(agent_client_message::Message::RunRequest(run_request)),
            })
            .await
            .map_err(|_| AgentServiceTransportError::OutboundClosed)?;

        let response = self
            .client
            .run(Request::new(ReceiverStream::new(outbound_rx)))
            .await
            .map_err(|status| AgentServiceTransportError::Rpc(status.to_string()))?;
        let heartbeat_cancel = CancellationToken::new();
        let heartbeat_task = spawn_heartbeat(outbound.clone(), heartbeat_cancel.clone());

        Ok(AgentServiceRun {
            outbound,
            inbound: response.into_inner(),
            heartbeat_cancel,
            heartbeat_task,
            state: RunState::Active,
        })
    }
}

pub struct AgentServiceRun {
    outbound: mpsc::Sender<AgentClientMessage>,
    inbound: Streaming<AgentServerMessage>,
    heartbeat_cancel: CancellationToken,
    heartbeat_task: JoinHandle<()>,
    state: RunState,
}

impl AgentServiceRun {
    pub async fn send_exec_client_message(
        &self,
        message: crate::proto::ExecClientMessage,
    ) -> Result<(), AgentServiceTransportError> {
        if self.state != RunState::Active {
            return Err(AgentServiceTransportError::RunClosed);
        }
        self.outbound
            .send(AgentClientMessage {
                message: Some(agent_client_message::Message::ExecClientMessage(message)),
            })
            .await
            .map_err(|_| AgentServiceTransportError::OutboundClosed)
    }

    pub async fn next_server_message(
        &mut self,
    ) -> Result<AgentServerMessage, AgentServiceTransportError> {
        if self.state != RunState::Active {
            return Err(AgentServiceTransportError::RunClosed);
        }

        let message = match self.inbound.message().await {
            Ok(Some(message)) => message,
            Ok(None) => return self.fail(AgentServiceTransportError::UnexpectedEof),
            Err(status) => {
                return self.fail(AgentServiceTransportError::Rpc(status.to_string()));
            }
        };
        if let Err(error) = validate_server_message(&message) {
            return self.fail(error);
        }
        if is_terminal(&message) {
            self.state = RunState::Ended;
            self.heartbeat_cancel.cancel();
        }
        Ok(message)
    }

    fn fail<T>(
        &mut self,
        error: AgentServiceTransportError,
    ) -> Result<T, AgentServiceTransportError> {
        self.state = RunState::Failed;
        self.heartbeat_cancel.cancel();
        Err(error)
    }
}

impl Drop for AgentServiceRun {
    fn drop(&mut self) {
        self.heartbeat_cancel.cancel();
        self.heartbeat_task.abort();
    }
}

fn spawn_heartbeat(
    outbound: mpsc::Sender<AgentClientMessage>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let heartbeat_interval = Duration::from_secs(HEARTBEAT_INTERVAL_SECONDS);
        let mut interval = tokio::time::interval(heartbeat_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = interval.tick() => {
                    let heartbeat = AgentClientMessage {
                        message: Some(agent_client_message::Message::ClientHeartbeat(
                            ClientHeartbeat {},
                        )),
                    };
                    if outbound.send(heartbeat).await.is_err() {
                        return;
                    }
                }
            }
        }
    })
}

fn validate_server_message(message: &AgentServerMessage) -> Result<(), AgentServiceTransportError> {
    match message.message.as_ref() {
        None => Err(AgentServiceTransportError::EmptyServerMessage),
        Some(agent_server_message::Message::InteractionUpdate(update)) => {
            validate_interaction_update(update)
        }
        Some(agent_server_message::Message::ExecServerMessage(exec)) => {
            validate_exec_server_message(exec)
        }
        Some(
            agent_server_message::Message::ConversationCheckpointUpdate(_)
            | agent_server_message::Message::KvServerMessage(_)
            | agent_server_message::Message::ExecServerControlMessage(_)
            | agent_server_message::Message::InteractionQuery(_),
        ) => Err(AgentServiceTransportError::UnsupportedServerMessage),
    }
}

fn validate_interaction_update(
    update: &crate::proto::InteractionUpdate,
) -> Result<(), AgentServiceTransportError> {
    match update.message.as_ref() {
        None => Err(AgentServiceTransportError::EmptyInteractionUpdate),
        Some(
            interaction_update::Message::TextDelta(_)
            | interaction_update::Message::Heartbeat(_)
            | interaction_update::Message::TurnEnded(_),
        ) => Ok(()),
        Some(
            interaction_update::Message::ToolCallStarted(_)
            | interaction_update::Message::ToolCallCompleted(_)
            | interaction_update::Message::ThinkingDelta(_)
            | interaction_update::Message::ThinkingCompleted(_)
            | interaction_update::Message::UserMessageAppended(_)
            | interaction_update::Message::PartialToolCall(_)
            | interaction_update::Message::TokenDelta(_)
            | interaction_update::Message::Summary(_)
            | interaction_update::Message::SummaryStarted(_)
            | interaction_update::Message::SummaryCompleted(_)
            | interaction_update::Message::ShellOutputDelta(_)
            | interaction_update::Message::ToolCallDelta(_)
            | interaction_update::Message::StepStarted(_)
            | interaction_update::Message::StepCompleted(_)
            | interaction_update::Message::PromptSuggestion(_)
            | interaction_update::Message::PostRequestPrompt(_)
            | interaction_update::Message::ActiveBranchChange(_)
            | interaction_update::Message::FeedbackRequest(_)
            | interaction_update::Message::ResponseComparison(_),
        ) => Err(AgentServiceTransportError::UnsupportedInteractionUpdate),
    }
}

fn validate_exec_server_message(
    exec: &crate::proto::ExecServerMessage,
) -> Result<(), AgentServiceTransportError> {
    match exec.message.as_ref() {
        None => Err(AgentServiceTransportError::EmptyExecServerMessage),
        Some(
            exec_server_message::Message::RequestContextArgs(_)
            | exec_server_message::Message::McpArgs(_),
        ) => Ok(()),
        Some(
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
        ) => Err(AgentServiceTransportError::UnsupportedExecServerMessage),
    }
}

fn is_terminal(message: &AgentServerMessage) -> bool {
    matches!(
        message.message.as_ref(),
        Some(agent_server_message::Message::InteractionUpdate(update))
            if matches!(
                update.message,
                Some(interaction_update::Message::TurnEnded(_))
            )
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunState {
    Active,
    Ended,
    Failed,
}
