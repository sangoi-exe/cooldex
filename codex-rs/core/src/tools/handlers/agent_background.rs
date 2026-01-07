use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::client_common::AGENT_RUN_PROMPT;
use crate::codex::Codex;
use crate::codex::CodexSpawnOk;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::protocol::InitialHistory;
use crate::protocol::Op;
use crate::protocol::SessionSource;
use crate::protocol::SubAgentSource;
use crate::subagent_runner::AgentRegistry;
use crate::subagent_runner::BackgroundAgentHandle;
use crate::subagent_runner::BackgroundAgentSnapshot;
use crate::subagent_runner::BackgroundAgentStatus;
use crate::subagent_runner::spawn_background_agent_loop;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::agent_run::default_agent_run_result_schema;
use crate::tools::handlers::agent_run::ensure_approval_policy_never;
use crate::tools::handlers::agent_run::inherit_effective_turn_settings;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::ConversationId;

const DEFAULT_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const DEFAULT_MAX_RESULT_BYTES: usize = 32 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentSpawnArgs {
    prompt: String,
    #[serde(default)]
    result_schema: Option<Value>,
    #[serde(default)]
    max_result_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentWaitArgs {
    agent_id: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentIdArgs {
    agent_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct AgentSpawnOutput {
    status: &'static str,
    agent_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct AgentWaitOutput {
    status: AgentWaitStatus,
    agent_id: String,
    #[serde(flatten)]
    snapshot: BackgroundAgentSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct AgentStatusOutput {
    agent_id: String,
    #[serde(flatten)]
    snapshot: BackgroundAgentSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct AgentCancelOutput {
    agent_id: String,
    #[serde(flatten)]
    snapshot: BackgroundAgentSnapshot,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum AgentWaitStatus {
    Completed,
    Errored,
    Aborted,
    Running,
    Timeout,
}

pub struct AgentSpawnHandler;
pub struct AgentWaitHandler;
pub struct AgentStatusHandler;
pub struct AgentCancelHandler;

#[async_trait]
impl ToolHandler for AgentSpawnHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn uses_workspace_lock(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "agent_spawn handler received unsupported payload".to_string(),
                ));
            }
        };

        let args = serde_json::from_str::<AgentSpawnArgs>(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
        })?;

        if args.prompt.trim().is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "agent_spawn requires non-empty prompt".to_string(),
            ));
        }

        ensure_approval_policy_never(turn.as_ref())?;

        let max_result_bytes = args
            .max_result_bytes
            .unwrap_or(DEFAULT_MAX_RESULT_BYTES as u64)
            .min(usize::MAX as u64) as usize;

        let mut config = (*session.original_config().await).clone();
        inherit_effective_turn_settings(&mut config, turn.as_ref())?;

        // Prevent runaway recursion (sub-agents spawning more sub-agents).
        config.features.disable(Feature::MultiAgent);

        let result_schema = args
            .result_schema
            .unwrap_or_else(default_agent_run_result_schema);

        let input = vec![codex_protocol::user_input::UserInput::Text {
            text: format!("{AGENT_RUN_PROMPT}\n\n## Task\n{}", args.prompt),
        }];

        let CodexSpawnOk {
            codex,
            conversation_id,
        } = Codex::spawn(
            config,
            Arc::clone(&session.services.auth_manager),
            Arc::clone(&session.services.models_manager),
            Arc::clone(&session.services.skills_manager),
            InitialHistory::New,
            SessionSource::SubAgent(SubAgentSource::Other("agent_spawn".to_string())),
            session.services.agent_control.clone(),
        )
        .await
        .map_err(|err| FunctionCallError::Fatal(format!("failed to spawn sub-agent: {err}")))?;

        let codex = Arc::new(codex);
        codex
            .submit(Op::UserInput {
                items: input,
                final_output_json_schema: Some(result_schema),
            })
            .await
            .map_err(|err| {
                FunctionCallError::Fatal(format!("failed to submit input to sub-agent: {err}"))
            })?;

        let handle = BackgroundAgentHandle::new(Arc::clone(&codex), max_result_bytes);
        session
            .services
            .agent_registry
            .insert(conversation_id, Arc::clone(&handle))
            .await;

        tokio::spawn(spawn_background_agent_loop(
            Arc::clone(&handle),
            Arc::clone(&session.services.agent_registry),
            conversation_id,
        ));

        let output = AgentSpawnOutput {
            status: "spawned",
            agent_id: conversation_id.to_string(),
        };

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&output).unwrap_or_else(|err| {
                format!(
                    "{{\"status\":\"errored\",\"error\":\"failed to serialize agent_spawn output: {err}\"}}"
                )
            }),
            content_items: None,
            success: Some(true),
        })
    }
}

#[async_trait]
impl ToolHandler for AgentWaitHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn uses_workspace_lock(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "agent_wait handler received unsupported payload".to_string(),
                ));
            }
        };

        let args = serde_json::from_str::<AgentWaitArgs>(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
        })?;

        ensure_approval_policy_never(turn.as_ref())?;

        let agent_id = parse_agent_id(&args.agent_id)?;
        let handle = get_agent_handle(&session.services.agent_registry, &agent_id).await?;

        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let finished = handle
            .wait_for_done(Duration::from_millis(timeout_ms))
            .await;

        let snapshot = handle.snapshot().await;
        let status = if finished {
            match snapshot.status {
                BackgroundAgentStatus::Completed => AgentWaitStatus::Completed,
                BackgroundAgentStatus::Errored => AgentWaitStatus::Errored,
                BackgroundAgentStatus::Aborted => AgentWaitStatus::Aborted,
                BackgroundAgentStatus::Running => AgentWaitStatus::Running,
            }
        } else {
            AgentWaitStatus::Timeout
        };

        let output = AgentWaitOutput {
            status,
            agent_id: args.agent_id,
            snapshot,
        };

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&output).unwrap_or_else(|err| {
                format!(
                    "{{\"status\":\"errored\",\"error\":\"failed to serialize agent_wait output: {err}\"}}"
                )
            }),
            content_items: None,
            success: Some(matches!(status, AgentWaitStatus::Completed)),
        })
    }
}

#[async_trait]
impl ToolHandler for AgentStatusHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn uses_workspace_lock(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "agent_status handler received unsupported payload".to_string(),
                ));
            }
        };

        let args = serde_json::from_str::<AgentIdArgs>(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
        })?;

        ensure_approval_policy_never(turn.as_ref())?;

        let agent_id = parse_agent_id(&args.agent_id)?;
        let handle = get_agent_handle(&session.services.agent_registry, &agent_id).await?;
        let snapshot = handle.snapshot().await;

        let output = AgentStatusOutput {
            agent_id: args.agent_id,
            snapshot,
        };

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&output).unwrap_or_else(|err| {
                format!(
                    "{{\"status\":\"errored\",\"error\":\"failed to serialize agent_status output: {err}\"}}"
                )
            }),
            content_items: None,
            success: Some(true),
        })
    }
}

#[async_trait]
impl ToolHandler for AgentCancelHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn uses_workspace_lock(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "agent_cancel handler received unsupported payload".to_string(),
                ));
            }
        };

        let args = serde_json::from_str::<AgentIdArgs>(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
        })?;

        ensure_approval_policy_never(turn.as_ref())?;

        let agent_id = parse_agent_id(&args.agent_id)?;
        let handle = get_agent_handle(&session.services.agent_registry, &agent_id).await?;
        handle.cancel().await;
        let snapshot = handle.snapshot().await;
        session.services.agent_registry.remove(&agent_id).await;

        let output = AgentCancelOutput {
            agent_id: args.agent_id,
            snapshot,
        };

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&output).unwrap_or_else(|err| {
                format!(
                    "{{\"status\":\"errored\",\"error\":\"failed to serialize agent_cancel output: {err}\"}}"
                )
            }),
            content_items: None,
            success: Some(true),
        })
    }
}

fn parse_agent_id(agent_id: &str) -> Result<ConversationId, FunctionCallError> {
    ConversationId::from_string(agent_id)
        .map_err(|err| FunctionCallError::RespondToModel(format!("invalid agent_id: {err}")))
}

async fn get_agent_handle(
    registry: &AgentRegistry,
    agent_id: &ConversationId,
) -> Result<Arc<BackgroundAgentHandle>, FunctionCallError> {
    registry
        .get(agent_id)
        .await
        .ok_or_else(|| FunctionCallError::RespondToModel("agent not found".to_string()))
}
