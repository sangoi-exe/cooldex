use super::CursorMappingError;
use crate::proto::McpResult;
use crate::proto::McpSuccess;
use crate::proto::McpTextContent;
use crate::proto::McpToolResult;
use crate::proto::McpToolResultContentItem;
use crate::proto::TurnEndedUpdate;
use crate::proto::mcp_result;
use crate::proto::mcp_tool_result;
use crate::proto::mcp_tool_result_content_item;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::TokenUsage;
use prost_types::ListValue;
use prost_types::Struct;
use prost_types::Value as ProstValue;
use prost_types::value::Kind as ProstValueKind;
use serde_json::Map as JsonMap;
use serde_json::Number as JsonNumber;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::HashMap;

pub(super) fn parse_json_object(
    arguments: &str,
    call_id: &str,
) -> Result<JsonMap<String, JsonValue>, CursorMappingError> {
    match serde_json::from_str(arguments) {
        Ok(JsonValue::Object(arguments)) => Ok(arguments),
        Ok(_) => Err(CursorMappingError::InvalidToolArguments {
            call_id: call_id.to_string(),
            reason: "expected object".to_string(),
        }),
        Err(error) => Err(CursorMappingError::InvalidToolArguments {
            call_id: call_id.to_string(),
            reason: error.to_string(),
        }),
    }
}

pub(super) fn extract_apply_patch_input(
    arguments: &JsonMap<String, JsonValue>,
) -> Result<String, CursorMappingError> {
    if arguments.len() != 1 {
        return Err(CursorMappingError::InvalidApplyPatchArguments);
    }
    match arguments.get("patch") {
        Some(JsonValue::String(patch)) => Ok(patch.clone()),
        _ => Err(CursorMappingError::InvalidApplyPatchArguments),
    }
}

pub(super) fn map_tool_output(
    output: &FunctionCallOutputPayload,
) -> Result<McpResult, CursorMappingError> {
    Ok(McpResult {
        result: Some(mcp_result::Result::Success(map_output_success(output)?)),
    })
}

pub(super) fn map_historical_tool_output(
    output: &FunctionCallOutputPayload,
) -> Result<McpToolResult, CursorMappingError> {
    Ok(McpToolResult {
        result: Some(mcp_tool_result::Result::Success(map_output_success(
            output,
        )?)),
    })
}

pub(super) fn map_token_usage(usage: &TurnEndedUpdate) -> Option<TokenUsage> {
    let has_usage = usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cache_read_tokens.is_some()
        || usage.cache_write_tokens.is_some()
        || usage.reasoning_tokens.is_some();
    has_usage.then(|| {
        let input_tokens = usage.input_tokens.unwrap_or_default();
        let output_tokens = usage.output_tokens.unwrap_or_default();
        TokenUsage {
            input_tokens,
            cached_input_tokens: usage.cache_read_tokens.unwrap_or_default(),
            cache_write_input_tokens: usage.cache_write_tokens.unwrap_or_default(),
            output_tokens,
            reasoning_output_tokens: usage.reasoning_tokens.unwrap_or_default(),
            total_tokens: input_tokens.saturating_add(output_tokens),
        }
    })
}

pub(super) fn json_object_to_prost_struct(value: &JsonValue) -> Result<Struct, String> {
    let JsonValue::Object(object) = value else {
        return Err("expected object".to_string());
    };
    json_map_to_prost_struct(object.clone())
}

pub(super) fn json_map_to_prost_map(
    object: JsonMap<String, JsonValue>,
) -> Result<HashMap<String, ProstValue>, String> {
    object
        .into_iter()
        .map(|(name, value)| Ok((name, json_value_to_prost(value)?)))
        .collect()
}

pub(super) fn prost_map_to_json_object(
    values: &HashMap<String, ProstValue>,
) -> Result<JsonMap<String, JsonValue>, String> {
    values
        .iter()
        .map(|(name, value)| Ok((name.clone(), prost_value_to_json(value)?)))
        .collect()
}

fn map_output_success(
    output: &FunctionCallOutputPayload,
) -> Result<McpSuccess, CursorMappingError> {
    let mut structured_content = None;
    let content = match &output.body {
        FunctionCallOutputBody::Text(text) => {
            if let Ok(JsonValue::Object(object)) = serde_json::from_str(text) {
                structured_content = Some(
                    json_map_to_prost_struct(object)
                        .map_err(|_| CursorMappingError::UnsupportedToolOutput)?,
                );
            }
            vec![McpToolResultContentItem {
                content: Some(mcp_tool_result_content_item::Content::Text(
                    McpTextContent { text: text.clone() },
                )),
            }]
        }
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => Ok(McpToolResultContentItem {
                    content: Some(mcp_tool_result_content_item::Content::Text(
                        McpTextContent { text: text.clone() },
                    )),
                }),
                FunctionCallOutputContentItem::InputImage { .. }
                | FunctionCallOutputContentItem::InputAudio { .. }
                | FunctionCallOutputContentItem::EncryptedContent { .. } => {
                    Err(CursorMappingError::UnsupportedToolOutput)
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(McpSuccess {
        content,
        is_error: output.success == Some(false),
        structured_content,
    })
}

fn json_map_to_prost_struct(object: JsonMap<String, JsonValue>) -> Result<Struct, String> {
    Ok(Struct {
        fields: object
            .into_iter()
            .map(|(name, value)| Ok((name, json_value_to_prost(value)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?,
    })
}

fn json_value_to_prost(value: JsonValue) -> Result<ProstValue, String> {
    let kind = match value {
        JsonValue::Null => ProstValueKind::NullValue(0),
        JsonValue::Bool(value) => ProstValueKind::BoolValue(value),
        JsonValue::Number(value) => ProstValueKind::NumberValue(json_number_to_f64(&value)?),
        JsonValue::String(value) => ProstValueKind::StringValue(value),
        JsonValue::Array(values) => ProstValueKind::ListValue(ListValue {
            values: values
                .into_iter()
                .map(json_value_to_prost)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        JsonValue::Object(object) => ProstValueKind::StructValue(json_map_to_prost_struct(object)?),
    };
    Ok(ProstValue { kind: Some(kind) })
}

fn json_number_to_f64(number: &JsonNumber) -> Result<f64, String> {
    if let Some(value) = number.as_i64() {
        let converted = value as f64;
        if converted as i64 == value {
            return Ok(converted);
        }
    } else if let Some(value) = number.as_u64() {
        let converted = value as f64;
        if converted as u64 == value {
            return Ok(converted);
        }
    } else if let Some(value) = number.as_f64()
        && value.is_finite()
    {
        return Ok(value);
    }
    Err(format!(
        "number is not exactly representable by protobuf: {number}"
    ))
}

fn prost_value_to_json(value: &ProstValue) -> Result<JsonValue, String> {
    match value.kind.as_ref() {
        None => Err("protobuf value has no kind".to_string()),
        Some(ProstValueKind::NullValue(_)) => Ok(JsonValue::Null),
        Some(ProstValueKind::BoolValue(value)) => Ok(JsonValue::Bool(*value)),
        Some(ProstValueKind::NumberValue(value)) => JsonNumber::from_f64(*value)
            .map(JsonValue::Number)
            .ok_or_else(|| "protobuf number is not finite".to_string()),
        Some(ProstValueKind::StringValue(value)) => Ok(JsonValue::String(value.clone())),
        Some(ProstValueKind::ListValue(list)) => list
            .values
            .iter()
            .map(prost_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(JsonValue::Array),
        Some(ProstValueKind::StructValue(object)) => object
            .fields
            .iter()
            .map(|(name, value)| Ok((name.clone(), prost_value_to_json(value)?)))
            .collect::<Result<JsonMap<_, _>, String>>()
            .map(JsonValue::Object),
    }
}
