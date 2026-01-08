use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::client_common::AGENT_RUN_PROMPT;
use crate::codex::Codex;
use crate::codex::CodexSpawnOk;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::protocol::AskForApproval;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::InitialHistory;
use crate::protocol::Op;
use crate::protocol::SessionSource;
use crate::protocol::SubAgentSource;
use crate::subagent_runner::maybe_route_subagent_approval;
use crate::subagent_runner::shutdown_subagent;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

const DEFAULT_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const DEFAULT_MAX_RESULT_BYTES: usize = 32 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRunArgs {
    prompt: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    result_schema: Option<Value>,
    #[serde(default)]
    max_result_bytes: Option<u64>,
}

struct AgentRunRunParams {
    timeout_duration: Duration,
    max_result_bytes: usize,
    input: Vec<codex_protocol::user_input::UserInput>,
    result_schema: Value,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum AgentRunStatus {
    Completed,
    Errored,
    Aborted,
    Timeout,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct AgentRunOutput {
    status: AgentRunStatus,
    conversation_id: String,
    model: String,
    elapsed_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    rollout_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub struct AgentRunHandler;

#[async_trait]
impl ToolHandler for AgentRunHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn uses_workspace_lock(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        // The spawned agent may run arbitrary tools (including filesystem writes), so treat this as
        // mutating to respect the turn tool gate.
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
                    "agent_run handler received unsupported payload".to_string(),
                ));
            }
        };

        let args = serde_json::from_str::<AgentRunArgs>(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
        })?;

        if args.prompt.trim().is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "agent_run requires non-empty prompt".to_string(),
            ));
        }

        ensure_approval_policy_never(turn.as_ref())?;

        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let timeout_duration = Duration::from_millis(timeout_ms);
        let max_result_bytes = args
            .max_result_bytes
            .unwrap_or(DEFAULT_MAX_RESULT_BYTES as u64)
            .min(usize::MAX as u64) as usize;

        let parent_cancel = parent_turn_cancellation_token(&session, &turn.sub_id).await?;
        let cancel_token = parent_cancel.child_token();

        let mut config = (*session.original_config().await).clone();
        inherit_effective_turn_settings(&mut config, turn.as_ref())?;

        // Prevent runaway recursion (sub-agents spawning more sub-agents).
        config.features.disable(Feature::MultiAgent);

        let result_schema = args
            .result_schema
            .unwrap_or_else(default_agent_run_result_schema);
        validate_result_schema(&result_schema)?;

        let input = vec![codex_protocol::user_input::UserInput::Text {
            text: format!("{AGENT_RUN_PROMPT}\n\n## Task\n{}", args.prompt),
        }];

        let run_params = AgentRunRunParams {
            timeout_duration,
            max_result_bytes,
            input,
            result_schema,
        };

        let model = config
            .model
            .clone()
            .unwrap_or_else(|| turn.client.get_model());

        let CodexSpawnOk {
            codex,
            conversation_id,
        } = Codex::spawn(
            config,
            Arc::clone(&session.services.auth_manager),
            Arc::clone(&session.services.models_manager),
            Arc::clone(&session.services.skills_manager),
            InitialHistory::New,
            SessionSource::SubAgent(SubAgentSource::Other("agent_run".to_string())),
            session.services.agent_control.clone(),
        )
        .await
        .map_err(|err| FunctionCallError::Fatal(format!("failed to spawn sub-agent: {err}")))?;

        let start = Instant::now();
        let (status, rollout_path, result, error) = run_one_shot_agent(
            &codex,
            session.as_ref(),
            turn.as_ref(),
            &cancel_token,
            run_params,
        )
        .await;

        let output = AgentRunOutput {
            status,
            conversation_id: conversation_id.to_string(),
            model,
            elapsed_ms: start.elapsed().as_millis(),
            rollout_path: rollout_path.map(|p| p.display().to_string()),
            result,
            error,
        };

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&output).unwrap_or_else(|err| {
                format!(
                    "{{\"status\":\"errored\",\"error\":\"failed to serialize agent_run output: {err}\"}}"
                )
            }),
            content_items: None,
            success: Some(matches!(output.status, AgentRunStatus::Completed)),
        })
    }
}

pub(crate) fn inherit_effective_turn_settings(
    config: &mut crate::config::Config,
    turn: &crate::codex::TurnContext,
) -> Result<(), FunctionCallError> {
    config.cwd = turn.cwd.clone();
    config.model = Some(turn.client.get_model());
    config.model_provider = turn.client.get_provider();
    config.model_reasoning_effort = turn.client.get_reasoning_effort();
    config.model_reasoning_summary = turn.client.get_reasoning_summary();

    config
        .approval_policy
        .set(turn.approval_policy)
        .map_err(|err| {
            FunctionCallError::Fatal(format!(
                "agent_run: failed to inherit approval_policy: {err}"
            ))
        })?;

    config
        .sandbox_policy
        .set(turn.sandbox_policy.clone())
        .map_err(|err| {
            FunctionCallError::Fatal(format!(
                "agent_run: failed to inherit sandbox_policy: {err}"
            ))
        })?;

    Ok(())
}

pub(crate) fn ensure_approval_policy_never(
    turn: &crate::codex::TurnContext,
) -> Result<(), FunctionCallError> {
    if turn.approval_policy != AskForApproval::Never {
        return Err(FunctionCallError::RespondToModel(
            "sub-agent execution requires approval_policy=never.".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_result_schema(schema: &Value) -> Result<(), FunctionCallError> {
    let Some(obj) = schema.as_object() else {
        return Err(FunctionCallError::RespondToModel(
            "result_schema must be a JSON object".to_string(),
        ));
    };

    let ty = obj.get("type").and_then(|value| value.as_str());
    if ty != Some("object") {
        return Err(FunctionCallError::RespondToModel(
            "result_schema must have top-level \"type\": \"object\"".to_string(),
        ));
    }

    let properties = obj.get("properties");
    if properties.is_none() {
        return Err(FunctionCallError::RespondToModel(
            "result_schema must define \"properties\" for object schemas".to_string(),
        ));
    }
    if !matches!(properties, Some(Value::Object(_))) {
        return Err(FunctionCallError::RespondToModel(
            "result_schema \"properties\" must be an object".to_string(),
        ));
    }

    Ok(())
}

async fn parent_turn_cancellation_token(
    session: &crate::codex::Session,
    sub_id: &str,
) -> Result<CancellationToken, FunctionCallError> {
    let active = session.active_turn.lock().await;
    let Some(active) = active.as_ref() else {
        return Err(FunctionCallError::Fatal(
            "agent_run: missing active turn; cannot route approvals".to_string(),
        ));
    };
    let Some(task) = active.tasks.get(sub_id) else {
        return Err(FunctionCallError::Fatal(format!(
            "agent_run: could not find running task for sub_id={sub_id}"
        )));
    };
    Ok(task.cancellation_token.clone())
}

pub(crate) fn default_agent_run_result_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "next_steps"],
        "properties": {
            "summary": { "type": "string" },
            "key_points": { "type": "array", "items": { "type": "string" } },
            "next_steps": { "type": "array", "items": { "type": "string" } },
            "commands": { "type": "array", "items": { "type": "string" } },
            "files": { "type": "array", "items": { "type": "string" } },
            "open_questions": { "type": "array", "items": { "type": "string" } },
            "risks": { "type": "array", "items": { "type": "string" } },
            "extra": { "type": "object", "additionalProperties": true },
        }
    })
}

async fn run_one_shot_agent(
    codex: &Codex,
    parent_session: &crate::codex::Session,
    parent_turn: &crate::codex::TurnContext,
    cancel_token: &CancellationToken,
    run: AgentRunRunParams,
) -> (
    AgentRunStatus,
    Option<PathBuf>,
    Option<Value>,
    Option<String>,
) {
    let AgentRunRunParams {
        timeout_duration,
        max_result_bytes,
        input,
        result_schema,
    } = run;
    let mut rollout_path: Option<PathBuf> = None;

    // Drain initial SessionConfigured to capture rollout path, but do not depend on strict ordering.
    if let Ok(Event {
        id: _,
        msg: EventMsg::SessionConfigured(ev),
    }) = codex.next_event().await
    {
        rollout_path = Some(ev.rollout_path);
    }

    if let Err(err) = codex
        .submit(Op::UserInput {
            items: input,
            final_output_json_schema: Some(result_schema),
        })
        .await
    {
        shutdown_subagent(codex).await;
        return (
            AgentRunStatus::Errored,
            rollout_path,
            None,
            Some(format!("failed to submit input to sub-agent: {err}")),
        );
    }

    let outcome = timeout(timeout_duration, async {
        let cancelled = cancel_token.cancelled();
        tokio::pin!(cancelled);

        loop {
            tokio::select! {
                biased;
                _ = &mut cancelled => {
                    return (AgentRunStatus::Aborted, None, Some("cancelled".to_string()));
                }
                event = codex.next_event() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(err) => {
                            return (AgentRunStatus::Errored, None, Some(format!("failed to receive sub-agent event: {err}")));
                        }
                    };
                    if let EventMsg::SessionConfigured(ev) = &event.msg {
                        rollout_path = Some(ev.rollout_path.clone());
                        continue;
                    }

                    if maybe_route_subagent_approval(
                        codex,
                        parent_session,
                        parent_turn,
                        cancel_token,
                        &event,
                    )
                    .await
                    {
                        continue;
                    }

                    match event.msg {
                        EventMsg::TurnComplete(ev) => {
                            return (AgentRunStatus::Completed, ev.last_agent_message, None);
                        }
                        EventMsg::TurnAborted(ev) => {
                            return (
                                AgentRunStatus::Aborted,
                                None,
                                Some(format!("{:?}", ev.reason)),
                            );
                        }
                        EventMsg::Error(ev) => {
                            return (AgentRunStatus::Errored, None, Some(ev.message));
                        }
                        _ => {}
                    }
                }
            }
        }
    })
    .await;

    let (mut status, last_agent_message, mut error) = match outcome {
        Ok(inner) => inner,
        Err(_) => {
            cancel_token.cancel();
            shutdown_subagent(codex).await;
            return (
                AgentRunStatus::Timeout,
                rollout_path,
                None,
                Some(format!("timed out after {timeout_duration:?}")),
            );
        }
    };

    let mut result = None;
    if matches!(status, AgentRunStatus::Completed) {
        match (last_agent_message, error.take()) {
            (Some(text), None) => match serde_json::from_str::<Value>(&text) {
                Ok(value) => {
                    let serialized = serde_json::to_string(&value).unwrap_or_default();
                    if serialized.len() > max_result_bytes {
                        status = AgentRunStatus::Errored;
                        error = Some(format!(
                            "sub-agent result too large: {} bytes (limit: {max_result_bytes}); re-run with a more compact summary",
                            serialized.len(),
                        ));
                    } else {
                        result = Some(value);
                    }
                }
                Err(err) => {
                    status = AgentRunStatus::Errored;
                    let truncated = text.chars().take(2048).collect::<String>();
                    error = Some(format!(
                        "sub-agent returned non-JSON output: {err}; output (truncated): {truncated}"
                    ));
                }
            },
            (None, None) => {
                status = AgentRunStatus::Errored;
                error = Some("sub-agent completed without a final message".to_string());
            }
            (_, Some(err)) => {
                status = AgentRunStatus::Errored;
                error = Some(err);
            }
        }
    }

    shutdown_subagent(codex).await;

    (status, rollout_path, result, error)
}
