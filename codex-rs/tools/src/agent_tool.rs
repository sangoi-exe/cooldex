use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use codex_protocol::openai_models::ModelPreset;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

// Merge-safety anchor: legacy spawn_agent v1 exported metadata must stay aligned with the
// active profile-based runtime contract; do not reintroduce dead model/reasoning overrides,
// hardcoded spawn-authorization gates, or default worker write-owner policy.
const SPAWN_AGENT_INHERITED_MODEL_GUIDANCE: &str = "Spawned agents inherit your current model by default. Use an override only when this surface exposes one and the user explicitly asks for different child settings.";
const SPAWN_AGENT_MODEL_OVERRIDE_DESCRIPTION: &str = "Optional model override for the new agent. Leave unset to inherit the same model as the parent, which is the preferred default. Only set this when the user explicitly asks for a different model or the task clearly requires one.";

#[derive(Debug, Clone)]
pub struct SpawnAgentToolOptions<'a> {
    pub available_models: &'a [ModelPreset],
    pub agent_type_description: String,
    pub hide_agent_type_model_reasoning: bool,
    pub include_usage_hint: bool,
    pub usage_hint_text: Option<String>,
    pub max_concurrent_threads_per_session: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnAgentModelSelection {
    ProfileOnly,
    DirectOverrides,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitAgentTimeoutOptions {
    pub default_timeout_ms: i64,
    pub min_timeout_ms: i64,
    pub max_timeout_ms: i64,
}

pub fn create_spawn_agent_tool_v1(options: SpawnAgentToolOptions<'_>) -> ToolSpec {
    let return_value_description =
        "Returns the spawned agent id plus the user-facing nickname when available.";
    let available_models_description = (!options.hide_agent_type_model_reasoning).then(|| {
        spawn_agent_models_description(
            options.available_models,
            SpawnAgentModelSelection::ProfileOnly,
        )
    });
    let mut properties = spawn_agent_common_properties_v1(&options.agent_type_description);
    if options.hide_agent_type_model_reasoning {
        hide_spawn_agent_metadata_options_v1(&mut properties);
    }

    ToolSpec::Function(ResponsesApiTool {
        name: "spawn_agent".to_string(),
        description: spawn_agent_tool_description(
            available_models_description.as_deref(),
            return_value_description,
            options.include_usage_hint,
            options.usage_hint_text,
        ),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, /*required*/ None, Some(false.into())),
        output_schema: Some(spawn_agent_output_schema_v1()),
    })
}

pub fn create_spawn_agent_tool_v2(options: SpawnAgentToolOptions<'_>) -> ToolSpec {
    let available_models_description = (!options.hide_agent_type_model_reasoning).then(|| {
        spawn_agent_models_description(
            options.available_models,
            SpawnAgentModelSelection::DirectOverrides,
        )
    });
    let mut properties = spawn_agent_common_properties_v2(&options.agent_type_description);
    if options.hide_agent_type_model_reasoning {
        hide_spawn_agent_metadata_options_v2(&mut properties);
    }
    properties.insert(
        "task_name".to_string(),
        JsonSchema::string(Some(
            "Task name for the new agent. Use lowercase letters, digits, and underscores."
                .to_string(),
        )),
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "spawn_agent".to_string(),
        description: spawn_agent_tool_description_v2(
            available_models_description.as_deref(),
            options.include_usage_hint,
            options.usage_hint_text,
            options.max_concurrent_threads_per_session,
        ),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["task_name".to_string(), "message".to_string()]),
            Some(false.into()),
        ),
        output_schema: Some(spawn_agent_output_schema_v2(
            options.hide_agent_type_model_reasoning,
        )),
    })
}

pub fn create_send_input_tool_v1() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "target".to_string(),
            JsonSchema::string(Some("Agent id to message (from spawn_agent).".to_string())),
        ),
        (
            "message".to_string(),
            JsonSchema::string(Some(
                "Legacy plain-text message to send to the agent. Use either message or items."
                    .to_string(),
            )),
        ),
        ("items".to_string(), create_collab_input_items_schema()),
        (
            "interrupt".to_string(),
            JsonSchema::boolean(Some(
                "When true, stop the agent's current task and handle this immediately. When false (default), queue this message."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "send_input".to_string(),
        description: "Send a message to an existing agent. Use interrupt=true to redirect work immediately. You should reuse the agent by send_input if you believe your assigned task is highly dependent on the context of a previous task."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["target".to_string()]), Some(false.into())),
        output_schema: Some(send_input_output_schema()),
    })
}

pub fn create_send_message_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "target".to_string(),
            JsonSchema::string(Some(
                "Relative or canonical task name to message (from spawn_agent).".to_string(),
            )),
        ),
        (
            "message".to_string(),
            JsonSchema::string(Some(
                "Message text to queue on the target agent.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "send_message".to_string(),
        description: "Send a message to an existing agent. The message will be delivered promptly. Does not trigger a new turn."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["target".to_string(), "message".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_followup_task_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "target".to_string(),
            JsonSchema::string(Some(
                "Agent id or canonical task name to message (from spawn_agent).".to_string(),
            )),
        ),
        (
            "message".to_string(),
            JsonSchema::string(Some(
                "Message text to send to the target agent.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "followup_task".to_string(),
        description: "Send a message to an existing non-root target agent and trigger a turn in that target. If the target is currently mid-turn, the message is queued and will be used to start the target's next turn, after the current turn completes."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["target".to_string(), "message".to_string()]), Some(false.into())),
        output_schema: None,
    })
}

pub fn create_resume_agent_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "id".to_string(),
        JsonSchema::string(Some("Agent id to resume.".to_string())),
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "resume_agent".to_string(),
        description:
            "Resume a previously closed agent by id so it can receive send_input and wait_agent calls."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["id".to_string()]), Some(false.into())),
        output_schema: Some(resume_agent_output_schema()),
    })
}

// Merge-safety anchor: legacy wait_agent metadata must stay aligned with the runtime
// errored-short-circuit contract so explicit any_final/all_final waits never regress into
// unreachable blocking semantics.
pub fn create_wait_agent_tool_v1(options: WaitAgentTimeoutOptions) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: "wait_agent".to_string(),
        description: "Wait on the requested agent ids. Omit return_when for the timed convenience mode, or combine disable_timeout=true with return_when=any_final|all_final for a blocking final-status wait. Explicit waits short-circuit immediately if a requested agent is already or becomes errored."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: wait_agent_tool_parameters_v1(options),
        output_schema: Some(wait_output_schema_v1()),
    })
}

pub fn create_wait_agent_tool_v2(options: WaitAgentTimeoutOptions) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: "wait_agent".to_string(),
        description: "Wait for a mailbox update from any live agent, including queued messages and final-status notifications. Does not return the content; returns either a summary of which agents have updates (if any), or a timeout summary if no mailbox update arrives before the deadline."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: wait_agent_tool_parameters_v2(options),
        output_schema: Some(wait_output_schema_v2()),
    })
}

pub fn create_list_agents_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "path_prefix".to_string(),
        JsonSchema::string(Some(
            "Optional task-path prefix (not ending with trailing slash). Accepts the same relative or absolute task-path syntax."
                .to_string(),
        )),
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "list_agents".to_string(),
        description:
            "List live agents in the current root thread tree. Optionally filter by task-path prefix."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, /*required*/ None, Some(false.into())),
        output_schema: Some(list_agents_output_schema()),
    })
}

pub fn create_close_agent_tool_v1() -> ToolSpec {
    let properties = BTreeMap::from([(
        "target".to_string(),
        JsonSchema::string(Some("Agent id to close (from spawn_agent).".to_string())),
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "close_agent".to_string(),
        description: "Close an agent and any open descendants when they are no longer needed, and return the target agent's previous status before shutdown was requested. Don't keep agents open for too long if they are not needed anymore.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["target".to_string()]), Some(false.into())),
        output_schema: Some(close_agent_output_schema()),
    })
}

pub fn create_close_agent_tool_v2() -> ToolSpec {
    let properties = BTreeMap::from([(
        "target".to_string(),
        JsonSchema::string(Some(
            "Agent id or canonical task name to close (from spawn_agent).".to_string(),
        )),
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "close_agent".to_string(),
        description: "Close an agent and any open descendants when they are no longer needed, and return the target agent's previous status before shutdown was requested. Don't keep agents open for too long if they are not needed anymore.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["target".to_string()]), Some(false.into())),
        output_schema: Some(close_agent_output_schema()),
    })
}

fn agent_status_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "string",
                "enum": ["pending_init", "running", "interrupted", "shutdown", "not_found"]
            },
            {
                "type": "object",
                "properties": {
                    "completed": {
                        "type": ["string", "null"]
                    }
                },
                "required": ["completed"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "errored": {
                        "type": "string"
                    }
                },
                "required": ["errored"],
                "additionalProperties": false
            }
        ]
    })
}

fn spawn_agent_output_schema_v1() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agent_id": {
                "type": "string",
                "description": "Thread identifier for the spawned agent."
            },
            "nickname": {
                "type": ["string", "null"],
                "description": "User-facing nickname for the spawned agent when available."
            }
        },
        "required": ["agent_id", "nickname"],
        "additionalProperties": false
    })
}

fn spawn_agent_output_schema_v2(hide_agent_metadata: bool) -> Value {
    if hide_agent_metadata {
        return json!({
            "type": "object",
            "properties": {
                "task_name": {
                    "type": "string",
                    "description": "Canonical task name for the spawned agent."
                }
            },
            "required": ["task_name"],
            "additionalProperties": false
        });
    }

    json!({
        "type": "object",
        "properties": {
            "task_name": {
                "type": "string",
                "description": "Canonical task name for the spawned agent."
            },
            "nickname": {
                "type": ["string", "null"],
                "description": "User-facing nickname for the spawned agent when available."
            }
        },
        "required": ["task_name", "nickname"],
        "additionalProperties": false
    })
}

fn send_input_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "submission_id": {
                "type": "string",
                "description": "Identifier for the queued input submission."
            }
        },
        "required": ["submission_id"],
        "additionalProperties": false
    })
}

fn list_agents_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agents": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "Canonical task name for the agent when available, otherwise the agent id."
                        },
                        "agent_status": {
                            "description": "Last known status of the agent.",
                            "allOf": [agent_status_output_schema()]
                        },
                        "last_task_message": {
                            "type": ["string", "null"],
                            "description": "Most recent user or inter-agent instruction received by the agent, when available."
                        }
                    },
                    "required": ["agent_name", "agent_status", "last_task_message"],
                    "additionalProperties": false
                },
                "description": "Live agents visible in the current root thread tree."
            }
        },
        "required": ["agents"],
        "additionalProperties": false
    })
}

fn resume_agent_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": agent_status_output_schema()
        },
        "required": ["status"],
        "additionalProperties": false
    })
}

fn wait_output_schema_v1() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agents": {
                "type": "object",
                "description": "Current runtime states keyed by agent id.",
                "additionalProperties": {
                    "type": "object",
                    "properties": {
                        "status": agent_status_output_schema(),
                        "last_activity": {
                            "type": ["object", "null"],
                            "description": "Most recent observed activity for the agent, when available.",
                            "properties": {
                                "kind": {
                                    "type": "string",
                                    "enum": ["message", "reasoning", "command", "edit", "task", "status"]
                                },
                                "summary": {
                                    "type": "string",
                                    "description": "Short, user-facing summary of the observed activity."
                                },
                                "occurred_at": {
                                    "type": "integer",
                                    "description": "Unix timestamp in seconds when the activity was observed."
                                }
                            },
                            "required": ["kind", "summary", "occurred_at"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["status", "last_activity"],
                    "additionalProperties": false
                }
            },
            "timed_out": {
                "type": "boolean",
                "description": "Whether the wait call returned due to timeout before the requested completion condition was satisfied."
            }
        },
        "required": ["agents", "timed_out"],
        "additionalProperties": false
    })
}

fn wait_output_schema_v2() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "Brief wait summary without the agent's final content."
            },
            "timed_out": {
                "type": "boolean",
                "description": "Whether the wait call returned because no mailbox update arrived before the timeout."
            }
        },
        "required": ["message", "timed_out"],
        "additionalProperties": false
    })
}

fn close_agent_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "previous_status": {
                "description": "The agent status observed before shutdown was requested.",
                "allOf": [agent_status_output_schema()]
            }
        },
        "required": ["previous_status"],
        "additionalProperties": false
    })
}

fn create_collab_input_items_schema() -> JsonSchema {
    let properties = BTreeMap::from([
        (
            "type".to_string(),
            JsonSchema::string(Some(
                "Input item type: text, image, local_image, skill, or mention.".to_string(),
            )),
        ),
        (
            "text".to_string(),
            JsonSchema::string(Some("Text content when type is text.".to_string())),
        ),
        (
            "image_url".to_string(),
            JsonSchema::string(Some("Image URL when type is image.".to_string())),
        ),
        (
            "path".to_string(),
            JsonSchema::string(Some(
                "Path when type is local_image/skill, or structured mention target such as app://<connector-id> or plugin://<plugin-name>@<marketplace-name> when type is mention."
                    .to_string(),
            )),
        ),
        (
            "name".to_string(),
            JsonSchema::string(Some("Display name when type is skill or mention.".to_string())),
        ),
    ]);

    JsonSchema::array(JsonSchema::object(properties, /*required*/ None, Some(false.into())), Some(
            "Structured input items. Use this to pass explicit mentions (for example app:// connector paths)."
                .to_string(),
        ))
}

fn spawn_agent_common_properties_v1(agent_type_description: &str) -> BTreeMap<String, JsonSchema> {
    BTreeMap::from([
        (
            "message".to_string(),
            JsonSchema::string(Some(
                "Initial plain-text task for the new agent. Use either message or items."
                    .to_string(),
            )),
        ),
        ("items".to_string(), create_collab_input_items_schema()),
        (
            "agent_type".to_string(),
            JsonSchema::string(Some(agent_type_description.to_string())),
        ),
        (
            "fork_context".to_string(),
            JsonSchema::boolean(Some(
                "When true, fork the current thread history into the new agent before sending the initial prompt. This must be used when you want the new agent to have exactly the same context as you."
                    .to_string(),
            )),
        ),
        (
            "profile".to_string(),
            JsonSchema::string(Some(
                "Optional config profile selected for the spawned agent.".to_string(),
            )),
        ),
    ])
}

fn spawn_agent_common_properties_v2(agent_type_description: &str) -> BTreeMap<String, JsonSchema> {
    BTreeMap::from([
        (
            "message".to_string(),
            JsonSchema::string(Some("Initial plain-text task for the new agent.".to_string())),
        ),
        (
            "agent_type".to_string(),
            JsonSchema::string(Some(agent_type_description.to_string())),
        ),
        (
            "fork_turns".to_string(),
            JsonSchema::string(Some(
                "Optional number of turns to fork. Defaults to `all`. Use `none`, `all`, or a positive integer string such as `3` to fork only the most recent turns."
                    .to_string(),
            )),
        ),
        (
            "model".to_string(),
            JsonSchema::string(Some(
                SPAWN_AGENT_MODEL_OVERRIDE_DESCRIPTION.to_string(),
            )),
        ),
        (
            "reasoning_effort".to_string(),
            JsonSchema::string(Some(
                "Optional reasoning effort override for the new agent. Replaces the inherited reasoning effort."
                    .to_string(),
            )),
        ),
    ])
}

fn hide_spawn_agent_metadata_options_v1(properties: &mut BTreeMap<String, JsonSchema>) {
    properties.remove("agent_type");
    properties.remove("profile");
}

fn hide_spawn_agent_metadata_options_v2(properties: &mut BTreeMap<String, JsonSchema>) {
    properties.remove("agent_type");
    properties.remove("model");
    properties.remove("reasoning_effort");
}

fn spawn_agent_tool_description(
    available_models_description: Option<&str>,
    return_value_description: &str,
    include_usage_hint: bool,
    usage_hint_text: Option<String>,
) -> String {
    let agent_role_guidance = available_models_description.unwrap_or_default();

    let tool_description = format!(
        r#"
        {agent_role_guidance}
        Spawn a sub-agent for a well-scoped task. {return_value_description} {SPAWN_AGENT_INHERITED_MODEL_GUIDANCE}"#
    );

    if !include_usage_hint {
        return tool_description;
    }
    if let Some(usage_hint_text) = usage_hint_text {
        return format!(
            r#"
        {tool_description}
{usage_hint_text}"#
        );
    }
    tool_description
}

fn spawn_agent_tool_description_v2(
    available_models_description: Option<&str>,
    include_usage_hint: bool,
    usage_hint_text: Option<String>,
    max_concurrent_threads_per_session: Option<usize>,
) -> String {
    let agent_role_guidance = available_models_description.unwrap_or_default();
    let concurrency_guidance = max_concurrent_threads_per_session
        .map(|limit| {
            format!(
                "This session is configured with `max_concurrent_threads_per_session = {limit}` for concurrently open agent threads."
            )
        })
        .unwrap_or_default();

    let tool_description = format!(
        r#"
        {agent_role_guidance}
        Spawns an agent to work on the specified task. If your current task is `/root/task1` and you spawn_agent with task_name "task_3" the agent will have canonical task name `/root/task1/task_3`.
You are then able to refer to this agent as `task_3` or `/root/task1/task_3` interchangeably. However an agent `/root/task2/task_3` would only be able to communicate with this agent via its canonical name `/root/task1/task_3`.
The spawned agent will have the same tools as you and the ability to spawn its own subagents.
{SPAWN_AGENT_INHERITED_MODEL_GUIDANCE}
It will be able to send you and other running agents messages, and its final answer will be provided to you when it finishes.
The new agent's canonical task name will be provided to it along with the message.
{concurrency_guidance}"#
    );

    if !include_usage_hint {
        return tool_description;
    }
    if let Some(usage_hint_text) = usage_hint_text {
        return format!(
            r#"
        {tool_description}
{usage_hint_text}"#
        );
    }
    tool_description
}

fn spawn_agent_models_description(
    models: &[ModelPreset],
    selection: SpawnAgentModelSelection,
) -> String {
    let visible_models: Vec<&ModelPreset> =
        models.iter().filter(|model| model.show_in_picker).collect();
    if visible_models.is_empty() {
        return "No picker-visible model overrides are currently loaded.".to_string();
    }

    let model_lines = visible_models
        .into_iter()
        .map(|model| {
            let efforts = model
                .supported_reasoning_efforts
                .iter()
                .map(|preset| format!("{} ({})", preset.effort, preset.description))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "- {} (`{}`): {} Default reasoning effort: {}. Supported reasoning efforts: {}.",
                model.display_name,
                model.model,
                model.description,
                model.default_reasoning_effort,
                efforts
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let selection_text = match selection {
        SpawnAgentModelSelection::ProfileOnly => {
            "The model catalog below is informational only; legacy `spawn_agent` does not accept direct `model` or `reasoning_effort` arguments.\nUse `profile` when you need alternate child settings; otherwise the child inherits the lead's live model and reasoning."
        }
        SpawnAgentModelSelection::DirectOverrides => {
            "The model catalog below is informational; use the `model` and `reasoning_effort` arguments when you need direct child overrides.\nOmit them to inherit the lead's live model and reasoning."
        }
    };

    format!("### Informational model catalog\n{selection_text}\n{model_lines}")
}

fn wait_agent_tool_parameters_v1(options: WaitAgentTimeoutOptions) -> JsonSchema {
    let properties = BTreeMap::from([
        (
            "ids".to_string(),
            JsonSchema::array(
                JsonSchema::string(/*description*/ None),
                Some("Agent ids to wait on.".to_string()),
            ),
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::number(Some(format!(
                "Optional timeout in milliseconds. Defaults to {}, min {}, max {}. Prefer longer waits (minutes) to avoid busy polling.",
                options.default_timeout_ms, options.min_timeout_ms, options.max_timeout_ms,
            ))),
        ),
        (
            "disable_timeout".to_string(),
            JsonSchema::boolean(Some(
                "When true, disable the timeout and require return_when. Cannot be combined with timeout_ms."
                    .to_string(),
            )),
        ),
        (
            "return_when".to_string(),
            JsonSchema::string(Some(
                "Optional completion condition when disable_timeout=true: any_final waits for the next requested non-final agent to newly become final, while all_final waits for every requested agent to be final. Either explicit wait short-circuits immediately if a requested agent is already or becomes errored."
                    .to_string(),
            )),
        ),
    ]);

    JsonSchema::object(
        properties,
        Some(vec!["ids".to_string()]),
        Some(false.into()),
    )
}

fn wait_agent_tool_parameters_v2(options: WaitAgentTimeoutOptions) -> JsonSchema {
    let properties = BTreeMap::from([(
        "timeout_ms".to_string(),
        JsonSchema::number(Some(format!(
            "Optional timeout in milliseconds. Defaults to {}, min {}, max {}.",
            options.default_timeout_ms, options.min_timeout_ms, options.max_timeout_ms,
        ))),
    )]);

    JsonSchema::object(properties, /*required*/ None, Some(false.into()))
}

#[cfg(test)]
#[path = "agent_tool_tests.rs"]
mod tests;
