use super::APPLY_PATCH_TOOL_NAME;
use super::COOLDEX_MCP_SERVER_IDENTIFIER;
use super::CursorMappingError;
use super::MAX_TOOL_DESCRIPTION_BYTES;
use super::MAX_TOOL_NAME_BYTES;
use super::MAX_TOOL_SCHEMA_BYTES;
use super::MAX_TOTAL_TOOL_SCHEMA_BYTES;
use super::values::json_map_to_prost_map;
use super::values::json_object_to_prost_struct;
use crate::proto::McpArgs;
use crate::proto::McpToolDefinition;
use crate::proto::McpTools;
use codex_protocol::ToolName;
use codex_tools::AdditionalProperties;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use codex_tools::code_mode_name_for_tool_name;
use serde_json::Map as JsonMap;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct CursorToolSnapshot {
    definitions: Vec<McpToolDefinition>,
    tools: Vec<FrozenTool>,
    by_identity: HashMap<(String, String), usize>,
    by_source_name: HashMap<ToolName, usize>,
}

#[derive(Clone, Debug)]
pub(super) struct FrozenTool {
    pub source_name: ToolName,
    pub kind: FrozenToolKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FrozenToolKind {
    Function,
    ApplyPatch,
}

impl CursorToolSnapshot {
    pub fn from_specs(specs: &[ToolSpec]) -> Result<Self, CursorMappingError> {
        let mut builder = ToolSnapshotBuilder::default();
        for spec in specs {
            match spec {
                ToolSpec::Function(tool) => builder.push_function(
                    COOLDEX_MCP_SERVER_IDENTIFIER,
                    ToolName::plain(&tool.name),
                    tool,
                )?,
                ToolSpec::Namespace(namespace) => {
                    for namespace_tool in &namespace.tools {
                        match namespace_tool {
                            ResponsesApiNamespaceTool::Function(tool) => builder.push_function(
                                &namespace.name,
                                ToolName::namespaced(&namespace.name, &tool.name),
                                tool,
                            )?,
                        }
                    }
                }
                ToolSpec::Freeform(tool) if tool.name == APPLY_PATCH_TOOL_NAME => {
                    builder.push_apply_patch(&tool.description)?;
                }
                ToolSpec::Freeform(tool) => {
                    return Err(CursorMappingError::UnsupportedTool(tool.name.clone()));
                }
                ToolSpec::ToolSearch { .. } => {
                    return Err(CursorMappingError::UnsupportedTool(
                        "tool_search".to_string(),
                    ));
                }
                ToolSpec::WebSearch { .. } => {
                    return Err(CursorMappingError::UnsupportedTool(
                        "web_search".to_string(),
                    ));
                }
            }
        }
        Ok(builder.finish())
    }

    pub fn definitions(&self) -> &[McpToolDefinition] {
        &self.definitions
    }

    pub fn mcp_tools(&self) -> McpTools {
        McpTools {
            mcp_tools: self.definitions.clone(),
        }
    }

    pub(super) fn tool_for_identity(
        &self,
        args: &McpArgs,
    ) -> Result<&FrozenTool, CursorMappingError> {
        let key = (args.provider_identifier.clone(), args.tool_name.clone());
        let Some(index) = self.by_identity.get(&key).copied() else {
            return Err(CursorMappingError::InvalidToolIdentity);
        };
        if args.name != self.definitions[index].name {
            return Err(CursorMappingError::InvalidToolIdentity);
        }
        Ok(&self.tools[index])
    }

    pub(super) fn historical_args(
        &self,
        source_name: &ToolName,
        call_id: &str,
        arguments: JsonMap<String, JsonValue>,
    ) -> Result<(McpArgs, Option<String>, FrozenToolKind), CursorMappingError> {
        let Some(index) = self.by_source_name.get(source_name).copied() else {
            return Err(CursorMappingError::UnsupportedTool(source_name.to_string()));
        };
        let definition = &self.definitions[index];
        Ok((
            McpArgs {
                name: definition.name.clone(),
                args: json_map_to_prost_map(arguments).map_err(|reason| {
                    CursorMappingError::InvalidToolArguments {
                        call_id: call_id.to_string(),
                        reason,
                    }
                })?,
                tool_call_id: call_id.to_string(),
                provider_identifier: definition.provider_identifier.clone(),
                tool_name: definition.tool_name.clone(),
                smart_mode_approval: None,
                smart_mode_approval_only: false,
                skip_approval: false,
                server_identifier: COOLDEX_MCP_SERVER_IDENTIFIER.to_string(),
            },
            (!definition.description.is_empty()).then(|| definition.description.clone()),
            self.tools[index].kind,
        ))
    }
}

#[derive(Default)]
struct ToolSnapshotBuilder {
    definitions: Vec<McpToolDefinition>,
    tools: Vec<FrozenTool>,
    by_identity: HashMap<(String, String), usize>,
    by_source_name: HashMap<ToolName, usize>,
    wire_names: HashSet<String>,
    total_schema_bytes: usize,
}

impl ToolSnapshotBuilder {
    fn push_function(
        &mut self,
        provider_identifier: &str,
        source_name: ToolName,
        tool: &ResponsesApiTool,
    ) -> Result<(), CursorMappingError> {
        if tool.defer_loading == Some(true) {
            return Err(CursorMappingError::DeferredTool(source_name.to_string()));
        }
        if tool.output_schema.is_some() {
            return Err(CursorMappingError::UnsupportedOutputSchema(
                source_name.to_string(),
            ));
        }
        let schema = serde_json::to_value(&tool.parameters).map_err(|error| {
            CursorMappingError::InvalidToolSchema {
                tool: source_name.to_string(),
                reason: error.to_string(),
            }
        })?;
        self.push(
            provider_identifier,
            source_name,
            tool.description.clone(),
            normalize_empty_schema(schema),
            FrozenToolKind::Function,
        )
    }

    fn push_apply_patch(&mut self, description: &str) -> Result<(), CursorMappingError> {
        let mut properties = BTreeMap::new();
        properties.insert("patch".to_string(), JsonSchema::string(None));
        let schema = serde_json::to_value(JsonSchema::object(
            properties,
            Some(vec!["patch".to_string()]),
            Some(AdditionalProperties::Boolean(false)),
        ))
        .map_err(|error| CursorMappingError::InvalidToolSchema {
            tool: APPLY_PATCH_TOOL_NAME.to_string(),
            reason: error.to_string(),
        })?;
        self.push(
            COOLDEX_MCP_SERVER_IDENTIFIER,
            ToolName::plain(APPLY_PATCH_TOOL_NAME),
            description.to_string(),
            schema,
            FrozenToolKind::ApplyPatch,
        )
    }

    fn push(
        &mut self,
        provider_identifier: &str,
        source_name: ToolName,
        description: String,
        schema: JsonValue,
        kind: FrozenToolKind,
    ) -> Result<(), CursorMappingError> {
        let wire_name = code_mode_name_for_tool_name(&source_name);
        validate_tool_text(&wire_name, &description)?;
        let schema_json = serde_json::to_string(&schema).map_err(|error| {
            CursorMappingError::InvalidToolSchema {
                tool: source_name.to_string(),
                reason: error.to_string(),
            }
        })?;
        if schema_json.len() > MAX_TOOL_SCHEMA_BYTES {
            return Err(CursorMappingError::ToolSchemaTooLarge(
                source_name.to_string(),
            ));
        }
        self.total_schema_bytes = self
            .total_schema_bytes
            .checked_add(schema_json.len())
            .ok_or(CursorMappingError::TotalToolSchemaTooLarge)?;
        if self.total_schema_bytes > MAX_TOTAL_TOOL_SCHEMA_BYTES {
            return Err(CursorMappingError::TotalToolSchemaTooLarge);
        }

        let identity = (provider_identifier.to_string(), source_name.name.clone());
        if self.by_identity.contains_key(&identity) {
            return Err(CursorMappingError::DuplicateToolIdentity {
                provider_identifier: identity.0,
                tool_name: identity.1,
            });
        }
        if !self.wire_names.insert(wire_name.clone()) {
            return Err(CursorMappingError::DuplicateWireToolName(wire_name));
        }
        if self.by_source_name.contains_key(&source_name) {
            return Err(CursorMappingError::DuplicateToolIdentity {
                provider_identifier: provider_identifier.to_string(),
                tool_name: source_name.name.clone(),
            });
        }

        let input_schema = json_object_to_prost_struct(&schema).map_err(|reason| {
            CursorMappingError::InvalidToolSchema {
                tool: source_name.to_string(),
                reason,
            }
        })?;
        let index = self.definitions.len();
        self.definitions.push(McpToolDefinition {
            name: wire_name,
            description,
            input_schema: Some(input_schema),
            provider_identifier: provider_identifier.to_string(),
            tool_name: source_name.name.clone(),
            input_schema_json: Some(schema_json),
        });
        self.tools.push(FrozenTool {
            source_name: source_name.clone(),
            kind,
        });
        self.by_identity.insert(identity, index);
        self.by_source_name.insert(source_name, index);
        Ok(())
    }

    fn finish(self) -> CursorToolSnapshot {
        CursorToolSnapshot {
            definitions: self.definitions,
            tools: self.tools,
            by_identity: self.by_identity,
            by_source_name: self.by_source_name,
        }
    }
}

fn validate_tool_text(name: &str, description: &str) -> Result<(), CursorMappingError> {
    if name.is_empty() {
        return Err(CursorMappingError::EmptyToolName);
    }
    if name.len() > MAX_TOOL_NAME_BYTES {
        return Err(CursorMappingError::ToolNameTooLong(name.to_string()));
    }
    if description.len() > MAX_TOOL_DESCRIPTION_BYTES {
        return Err(CursorMappingError::ToolDescriptionTooLong(name.to_string()));
    }
    Ok(())
}

fn normalize_empty_schema(schema: JsonValue) -> JsonValue {
    if schema.as_object().is_some_and(JsonMap::is_empty) {
        let mut explicit = JsonMap::new();
        explicit.insert("type".to_string(), JsonValue::String("object".to_string()));
        explicit.insert("properties".to_string(), JsonValue::Object(JsonMap::new()));
        explicit.insert("required".to_string(), JsonValue::Array(Vec::new()));
        explicit.insert("additionalProperties".to_string(), JsonValue::Bool(false));
        JsonValue::Object(explicit)
    } else {
        schema
    }
}
