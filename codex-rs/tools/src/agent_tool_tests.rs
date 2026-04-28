// Merge-safety anchor: legacy collab tool-schema tests here are followers of the shipped V1
// agent-tool canon; merges must keep these expectations aligned with `agent_tool.rs` and the
// runtime handlers instead of letting stale aliases survive only in tests.

use super::*;
use crate::JsonSchemaPrimitiveType;
use crate::JsonSchemaType;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use pretty_assertions::assert_eq;
use serde_json::json;

fn model_preset(id: &str, show_in_picker: bool) -> ModelPreset {
    ModelPreset {
        id: id.to_string(),
        model: format!("{id}-model"),
        display_name: format!("{id} display"),
        description: format!("{id} description"),
        default_reasoning_effort: ReasoningEffort::Medium,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: "Balanced".to_string(),
        }],
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        is_default: false,
        upgrade: None,
        show_in_picker,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: Vec::new(),
    }
}

#[test]
fn spawn_agent_tool_v2_requires_task_name_and_lists_visible_models() {
    let tool = create_spawn_agent_tool_v2(SpawnAgentToolOptions {
        available_models: &[
            model_preset("visible", /*show_in_picker*/ true),
            model_preset("hidden", /*show_in_picker*/ false),
        ],
        agent_type_description: "role help".to_string(),
        hide_agent_type_model_reasoning: false,
        include_usage_hint: true,
        usage_hint_text: None,
    });

    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        output_schema,
        ..
    }) = tool
    else {
        panic!("spawn_agent should be a function tool");
    };
    assert_eq!(
        parameters.schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Object))
    );
    let properties = parameters
        .properties
        .as_ref()
        .expect("spawn_agent should use object params");
    assert!(description.contains("Spawns an agent to work on the specified task."));
    assert!(description.contains("The spawned agent will have the same tools as you"));
    assert!(description.contains(
        "The model catalog below is informational; use the `model` and `reasoning_effort` arguments when you need direct child overrides."
    ));
    assert!(description.contains("visible display (`visible-model`)"));
    assert!(!description.contains("hidden display (`hidden-model`)"));
    assert!(properties.contains_key("task_name"));
    assert!(properties.contains_key("message"));
    assert!(properties.contains_key("fork_turns"));
    assert!(properties.contains_key("model"));
    assert!(properties.contains_key("reasoning_effort"));
    assert!(!properties.contains_key("items"));
    assert!(!properties.contains_key("fork_context"));
    assert!(!description.contains(concat!(
        "does not accept direct `model` ",
        "or `reasoning_effort` arguments"
    )));
    assert_eq!(
        properties.get("agent_type"),
        Some(&JsonSchema::string(Some("role help".to_string())))
    );
    assert_eq!(
        parameters.required.as_ref(),
        Some(&vec!["task_name".to_string(), "message".to_string()])
    );
    assert_eq!(
        output_schema.expect("spawn_agent output schema")["required"],
        json!(["task_name", "nickname"])
    );
}

#[test]
fn spawn_agent_tool_v1_keeps_legacy_fork_context_field() {
    let tool = create_spawn_agent_tool_v1(SpawnAgentToolOptions {
        available_models: &[],
        agent_type_description: "role help".to_string(),
        hide_agent_type_model_reasoning: false,
        include_usage_hint: true,
        usage_hint_text: None,
    });

    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = tool else {
        panic!("spawn_agent should be a function tool");
    };
    assert_eq!(
        parameters.schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Object))
    );
    let properties = parameters
        .properties
        .as_ref()
        .expect("spawn_agent should use object params");

    assert!(properties.contains_key("fork_context"));
    assert!(properties.contains_key("profile"));
    assert!(!properties.contains_key("model"));
    assert!(!properties.contains_key("reasoning_effort"));
    assert!(!properties.contains_key("fork_turns"));
}

#[test]
fn spawn_agent_tool_v1_default_description_has_no_delegation_policy() {
    let tool = create_spawn_agent_tool_v1(SpawnAgentToolOptions {
        available_models: &[],
        agent_type_description: "role help".to_string(),
        hide_agent_type_model_reasoning: false,
        include_usage_hint: true,
        usage_hint_text: None,
    });

    let ToolSpec::Function(ResponsesApiTool { description, .. }) = tool else {
        panic!("spawn_agent should be a function tool");
    };

    assert!(description.contains("Spawn a sub-agent for a well-scoped task."));
    for stale_text in [
        concat!("Only use `spawn_agent` ", "if and only if"),
        concat!("Requests for depth, ", "thoroughness"),
        concat!("Agent-role guidance below ", "only helps choose"),
        concat!("prefer delegating concrete ", "code-change worker"),
        concat!("edit files directly ", "in its forked workspace"),
        concat!("For code-edit subtasks, ", "decompose work"),
        concat!("Split implementation into ", "disjoint codebase slices"),
    ] {
        assert!(
            !description.contains(stale_text),
            "spawn_agent V1 description should not contain stale policy {stale_text:?}: {description:?}"
        );
    }
}

#[test]
fn spawn_agent_tools_append_custom_usage_hint_text() {
    let usage_hint_text = Some("Custom delegation guidance.".to_string());
    let v1_tool = create_spawn_agent_tool_v1(SpawnAgentToolOptions {
        available_models: &[],
        agent_type_description: "role help".to_string(),
        hide_agent_type_model_reasoning: false,
        include_usage_hint: true,
        usage_hint_text: usage_hint_text.clone(),
    });
    let v2_tool = create_spawn_agent_tool_v2(SpawnAgentToolOptions {
        available_models: &[],
        agent_type_description: "role help".to_string(),
        hide_agent_type_model_reasoning: false,
        include_usage_hint: true,
        usage_hint_text,
    });

    let ToolSpec::Function(ResponsesApiTool {
        description: v1_description,
        ..
    }) = v1_tool
    else {
        panic!("spawn_agent V1 should be a function tool");
    };
    let ToolSpec::Function(ResponsesApiTool {
        description: v2_description,
        ..
    }) = v2_tool
    else {
        panic!("spawn_agent V2 should be a function tool");
    };

    assert!(v1_description.contains("Custom delegation guidance."));
    assert!(v2_description.contains("Custom delegation guidance."));
}

#[test]
fn send_message_tool_requires_message_and_has_no_output_schema() {
    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        output_schema,
        ..
    }) = create_send_message_tool()
    else {
        panic!("send_message should be a function tool");
    };
    assert_eq!(
        parameters.schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Object))
    );
    let properties = parameters
        .properties
        .as_ref()
        .expect("send_message should use object params");
    assert!(properties.contains_key("target"));
    assert!(properties.contains_key("message"));
    assert!(!properties.contains_key("interrupt"));
    assert!(!properties.contains_key("items"));
    assert_eq!(
        description,
        "Send a string message to an existing agent without triggering a new turn."
    );
    assert_eq!(
        properties
            .get("target")
            .and_then(|schema| schema.description.as_deref()),
        Some("Relative or canonical task name to message (from spawn_agent).")
    );
    assert_eq!(
        parameters.required.as_ref(),
        Some(&vec!["target".to_string(), "message".to_string()])
    );
    assert_eq!(output_schema, None);
}

#[test]
fn followup_task_tool_requires_message_and_has_no_output_schema() {
    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        output_schema,
        ..
    }) = create_followup_task_tool()
    else {
        panic!("followup_task should be a function tool");
    };
    assert_eq!(
        parameters.schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Object))
    );
    let properties = parameters
        .properties
        .as_ref()
        .expect("followup_task should use object params");
    assert!(properties.contains_key("target"));
    assert!(properties.contains_key("message"));
    assert!(properties.contains_key("interrupt"));
    assert!(!properties.contains_key("items"));
    assert!(description.contains(
        "Send a string message to an existing non-root agent and trigger a turn in the target."
    ));
    assert!(description.contains(
        "If interrupt=false and the target's turn has not completed, the message is queued"
    ));
    assert_eq!(
        properties
            .get("interrupt")
            .and_then(|schema| schema.description.as_deref()),
        Some(
            "When true, stop the agent's current task and handle this immediately. When false (default), queue this message; if the target is already running, it starts the target's next turn after the current turn completes."
        )
    );
    assert_eq!(
        parameters.required.as_ref(),
        Some(&vec!["target".to_string(), "message".to_string()])
    );
    assert_eq!(output_schema, None);
}

#[test]
fn wait_agent_tool_v1_exposes_ids_conditions_and_agents_output() {
    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        output_schema,
        ..
    }) = create_wait_agent_tool_v1(WaitAgentTimeoutOptions {
        default_timeout_ms: 30_000,
        min_timeout_ms: 10_000,
        max_timeout_ms: 3_600_000,
    })
    else {
        panic!("wait_agent should be a function tool");
    };
    let properties = parameters
        .properties
        .as_ref()
        .expect("wait_agent should use object params");
    assert!(properties.contains_key("ids"));
    assert!(properties.contains_key("timeout_ms"));
    assert!(properties.contains_key("disable_timeout"));
    assert!(properties.contains_key("return_when"));
    assert!(!properties.contains_key("targets"));
    assert_eq!(parameters.required.as_ref(), Some(&vec!["ids".to_string()]));
    assert_eq!(
        description,
        "Wait on the requested agent ids. Omit return_when for the timed convenience mode, or combine disable_timeout=true with return_when=any_final|all_final for a blocking final-status wait. Explicit waits short-circuit immediately if a requested agent is already or becomes errored."
    );
    assert_eq!(
        properties.get("return_when"),
        Some(&JsonSchema::string(Some(
            "Optional completion condition when disable_timeout=true: any_final waits for the next requested non-final agent to newly become final, while all_final waits for every requested agent to be final. Either explicit wait short-circuits immediately if a requested agent is already or becomes errored."
                .to_string()
        )))
    );

    let output_schema = output_schema.expect("wait output schema");
    assert_eq!(output_schema["required"], json!(["agents", "timed_out"]));
    assert_eq!(
        output_schema["properties"]["agents"]["additionalProperties"]["required"],
        json!(["status", "last_activity"])
    );
    assert_eq!(
        output_schema["properties"]["timed_out"]["description"],
        json!(
            "Whether the wait call returned due to timeout before the requested completion condition was satisfied."
        )
    );
}

#[test]
fn wait_agent_tool_v2_uses_timeout_only_summary_output() {
    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        output_schema,
        ..
    }) = create_wait_agent_tool_v2(WaitAgentTimeoutOptions {
        default_timeout_ms: 30_000,
        min_timeout_ms: 10_000,
        max_timeout_ms: 3_600_000,
    })
    else {
        panic!("wait_agent should be a function tool");
    };
    assert_eq!(
        parameters.schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Object))
    );
    let properties = parameters
        .properties
        .as_ref()
        .expect("wait_agent should use object params");
    assert!(!properties.contains_key("targets"));
    assert!(properties.contains_key("timeout_ms"));
    assert!(description.contains(
        "Does not return the content; returns either a summary of which agents have updates (if any)"
    ));
    assert_eq!(
        properties
            .get("timeout_ms")
            .and_then(|schema| schema.description.as_deref()),
        Some("Optional timeout in milliseconds. Defaults to 30000, min 10000, max 3600000.")
    );
    assert_eq!(parameters.required.as_ref(), None);
    assert_eq!(
        output_schema.expect("wait output schema")["properties"]["message"]["description"],
        json!("Brief wait summary without the agent's final content.")
    );
}

#[test]
fn list_agents_tool_includes_path_prefix_and_agent_fields() {
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = create_list_agents_tool()
    else {
        panic!("list_agents should be a function tool");
    };
    assert_eq!(
        parameters.schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Object))
    );
    let properties = parameters
        .properties
        .as_ref()
        .expect("list_agents should use object params");
    assert!(properties.contains_key("path_prefix"));
    assert_eq!(
        properties
            .get("path_prefix")
            .and_then(|schema| schema.description.as_deref()),
        Some(
            "Optional task-path prefix (not ending with trailing slash). Accepts the same relative or absolute task-path syntax."
        )
    );
    assert_eq!(
        output_schema.expect("list_agents output schema")["properties"]["agents"]["items"]["required"],
        json!(["agent_name", "agent_status", "last_task_message"])
    );
}

#[test]
fn list_agents_tool_status_schema_includes_interrupted() {
    let ToolSpec::Function(ResponsesApiTool { output_schema, .. }) = create_list_agents_tool()
    else {
        panic!("list_agents should be a function tool");
    };

    assert_eq!(
        output_schema.expect("list_agents output schema")["properties"]["agents"]["items"]["properties"]
            ["agent_status"]["allOf"][0]["oneOf"][0]["enum"],
        json!([
            "pending_init",
            "running",
            "interrupted",
            "shutdown",
            "not_found"
        ])
    );
}
