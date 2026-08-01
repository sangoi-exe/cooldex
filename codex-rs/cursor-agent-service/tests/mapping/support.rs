use codex_cursor_agent_service::CursorToolSnapshot;
use codex_cursor_agent_service::proto::McpArgs;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_tools::AdditionalProperties;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use prost_types::Struct;
use prost_types::Value as ProstValue;
use prost_types::value::Kind as ProstValueKind;
use serde_json::Number as JsonNumber;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::HashMap;

pub fn empty_object_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::new(),
        Some(Vec::new()),
        Some(AdditionalProperties::Boolean(false)),
    )
}

pub fn function_spec(name: &str) -> ToolSpec {
    function_spec_with(name, "A test function", empty_object_schema())
}

pub fn function_spec_with(name: &str, description: &str, parameters: JsonSchema) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: true,
        defer_loading: None,
        parameters,
        output_schema: None,
    })
}

pub fn namespace_spec(namespace: &str, tool_name: &str) -> ToolSpec {
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: namespace.to_string(),
        description: format!("Tools in {namespace}"),
        tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
            name: tool_name.to_string(),
            description: "A namespaced function".to_string(),
            strict: true,
            defer_loading: None,
            parameters: empty_object_schema(),
            output_schema: None,
        })],
    })
}

pub fn apply_patch_spec() -> ToolSpec {
    freeform_spec("apply_patch")
}

pub fn freeform_spec(name: &str) -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: name.to_string(),
        description: format!("Invoke {name}"),
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: "start: /.+/s".to_string(),
        },
    })
}

pub fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

pub fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

pub fn function_call(name: &str, namespace: Option<&str>, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        arguments: r#"{"value":"hello"}"#.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

pub fn function_output(call_id: &str, text: &str, success: Option<bool>) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: output(text, success),
        internal_chat_message_metadata_passthrough: None,
    }
}

pub fn apply_patch_call(call_id: &str, patch: &str) -> ResponseItem {
    ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: call_id.to_string(),
        name: "apply_patch".to_string(),
        namespace: None,
        input: patch.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

pub fn apply_patch_output(call_id: &str, name: Option<&str>, text: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: call_id.to_string(),
        name: name.map(str::to_string),
        output: output(text, Some(true)),
        internal_chat_message_metadata_passthrough: None,
    }
}

pub fn output(text: &str, success: Option<bool>) -> FunctionCallOutputPayload {
    FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(text.to_string()),
        success,
    }
}

pub fn valid_mcp_args(
    snapshot: &CursorToolSnapshot,
    definition_index: usize,
    action_id: &str,
) -> McpArgs {
    let definition = &snapshot.definitions()[definition_index];
    McpArgs {
        name: definition.name.clone(),
        args: HashMap::new(),
        tool_call_id: action_id.to_string(),
        provider_identifier: definition.provider_identifier.clone(),
        tool_name: definition.tool_name.clone(),
        smart_mode_approval: None,
        smart_mode_approval_only: false,
        skip_approval: false,
        server_identifier: "cooldex".to_string(),
    }
}

pub fn prost_string(value: &str) -> ProstValue {
    ProstValue {
        kind: Some(ProstValueKind::StringValue(value.to_string())),
    }
}

pub fn prost_struct_to_json(value: &Struct) -> JsonValue {
    JsonValue::Object(
        value
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), prost_value_to_json(value)))
            .collect(),
    )
}

fn prost_value_to_json(value: &ProstValue) -> JsonValue {
    match value
        .kind
        .as_ref()
        .expect("test protobuf value should have a kind")
    {
        ProstValueKind::NullValue(_) => JsonValue::Null,
        ProstValueKind::BoolValue(value) => JsonValue::Bool(*value),
        ProstValueKind::NumberValue(value) => JsonValue::Number(
            JsonNumber::from_f64(*value).expect("test protobuf number should be finite"),
        ),
        ProstValueKind::StringValue(value) => JsonValue::String(value.clone()),
        ProstValueKind::ListValue(value) => {
            JsonValue::Array(value.values.iter().map(prost_value_to_json).collect())
        }
        ProstValueKind::StructValue(value) => prost_struct_to_json(value),
    }
}
