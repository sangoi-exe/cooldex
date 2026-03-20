use super::*;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::apply_role_to_config;

use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;

pub(crate) struct Handler;

#[async_trait]
impl ToolHandler for Handler {
    type Output = SpawnAgentResult;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let args: SpawnAgentArgs = parse_arguments(&arguments)?;
        let role_name = args
            .agent_type
            .as_deref()
            .map(str::trim)
            .filter(|role| !role.is_empty());
        let profile_name = args
            .profile
            .as_deref()
            .map(str::trim)
            .filter(|profile| !profile.is_empty());
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let session_source = turn.session_source.clone();
        let child_depth = next_thread_spawn_depth(&session_source);
        let max_depth = turn.config.agent_max_depth;
        if exceeds_thread_spawn_depth_limit(child_depth, max_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }
        let mut config = build_agent_spawn_config(turn.as_ref())?;
        apply_spawn_agent_profile_override(&mut config, profile_name)?;
        apply_role_to_config(&mut config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
        finalize_spawn_agent_prompt_config(
            &mut config,
            turn.as_ref(),
            session.services.models_manager.as_ref(),
        )
        .await;
        apply_spawn_agent_overrides(&mut config, child_depth);
        // Merge-safety anchor: spawn-agent children inherit the lead model/reasoning unless a
        // profile replaces them, and role-locked settings still win after profile selection;
        // begin/end events must report the effective child profile/model/reasoning together.
        let configured_model = config
            .model
            .clone()
            .unwrap_or_else(|| turn.model_info.slug.clone());
        let configured_reasoning_effort = config.model_reasoning_effort;
        let configured_profile = config.active_profile.clone();
        session
            .send_event(
                &turn,
                CollabAgentSpawnBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    prompt: prompt.clone(),
                    profile: configured_profile.clone(),
                    model: configured_model.clone(),
                    reasoning_effort: configured_reasoning_effort,
                }
                .into(),
            )
            .await;

        let result = session
            .services
            .agent_control
            .spawn_agent_with_options(
                config,
                input_items,
                Some(thread_spawn_source(
                    session.conversation_id,
                    child_depth,
                    role_name,
                )),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: args.fork_context.then(|| call_id.clone()),
                },
            )
            .await
            .map_err(collab_spawn_error);
        let (new_thread_id, status, agent_snapshot) = match result.as_ref() {
            Ok(thread_id) => {
                let Some(agent_snapshot) = session
                    .services
                    .agent_control
                    .get_agent_config_snapshot(*thread_id)
                    .await
                else {
                    return Err(FunctionCallError::Fatal(format!(
                        "spawned agent {thread_id} missing config snapshot after successful spawn"
                    )));
                };
                (
                    Some(*thread_id),
                    session.services.agent_control.get_status(*thread_id).await,
                    Some(agent_snapshot),
                )
            }
            Err(_) => (None, AgentStatus::NotFound, None),
        };
        let (new_agent_nickname, new_agent_role) = match &agent_snapshot {
            Some(snapshot) => (
                snapshot.session_source.get_nickname(),
                snapshot.session_source.get_agent_role(),
            ),
            None => (None, None),
        };
        let effective_model = agent_snapshot
            .as_ref()
            .map(|snapshot| snapshot.model.clone())
            .unwrap_or_else(|| configured_model.clone());
        let effective_reasoning_effort = agent_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.reasoning_effort)
            .or(configured_reasoning_effort);
        let nickname = new_agent_nickname.clone();
        session
            .send_event(
                &turn,
                CollabAgentSpawnEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    new_thread_id,
                    new_agent_nickname,
                    new_agent_role,
                    prompt,
                    profile: configured_profile,
                    model: effective_model,
                    reasoning_effort: effective_reasoning_effort,
                    status,
                }
                .into(),
            )
            .await;
        let new_thread_id = result?;
        let role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);
        turn.session_telemetry.counter(
            "codex.multi_agent.spawn",
            /*inc*/ 1,
            &[("role", role_tag)],
        );

        Ok(SpawnAgentResult {
            agent_id: new_thread_id.to_string(),
            nickname,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    message: Option<String>,
    items: Option<Vec<UserInput>>,
    agent_type: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    fork_context: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SpawnAgentResult {
    agent_id: String,
    nickname: Option<String>,
}

impl ToolOutput for SpawnAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "spawn_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "spawn_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "spawn_agent")
    }
}
