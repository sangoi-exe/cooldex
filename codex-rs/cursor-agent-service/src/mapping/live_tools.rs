use super::APPLY_PATCH_TOOL_NAME;
use super::COOLDEX_MCP_SERVER_IDENTIFIER;
use super::CursorMappingError;
use super::tools::CursorToolSnapshot;
use super::tools::FrozenToolKind;
use super::values::extract_apply_patch_input;
use super::values::map_tool_output;
use super::values::prost_map_to_json_object;
use crate::proto::ExecClientMessage;
use crate::proto::McpArgs;
use crate::proto::exec_client_message;
use codex_protocol::ToolName;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Debug)]
pub struct CursorToolCallTracker {
    snapshot: CursorToolSnapshot,
    max_pending: usize,
    pending: HashMap<String, PendingLiveCall>,
    seen_action_ids: HashSet<String>,
    seen_cooldex_call_ids: HashSet<String>,
    completed_action_ids: HashSet<String>,
}

#[derive(Debug)]
struct PendingLiveCall {
    exec_message_id: u32,
    exec_id: String,
    cooldex_call_id: String,
}

#[derive(Debug, PartialEq)]
pub struct AcceptedLiveToolCall {
    pub cursor_action_id: String,
    pub cooldex_call_id: String,
    pub tool_name: ToolName,
    pub response_item: ResponseItem,
}

#[derive(Debug, PartialEq)]
pub struct CompletedLiveToolCall {
    pub cooldex_call_id: String,
    pub exec_client_message: ExecClientMessage,
}

impl CursorToolCallTracker {
    pub fn new(snapshot: CursorToolSnapshot, max_pending: usize) -> Self {
        Self {
            snapshot,
            max_pending,
            pending: HashMap::new(),
            seen_action_ids: HashSet::new(),
            seen_cooldex_call_ids: HashSet::new(),
            completed_action_ids: HashSet::new(),
        }
    }

    pub fn accept_mcp_call(
        &mut self,
        exec_message_id: u32,
        exec_id: String,
        args: McpArgs,
        cooldex_call_id: String,
    ) -> Result<AcceptedLiveToolCall, CursorMappingError> {
        validate_live_mcp_args(&args)?;
        if self.pending.len() >= self.max_pending {
            return Err(CursorMappingError::PendingToolLimit(self.max_pending));
        }
        if self.seen_action_ids.contains(&args.tool_call_id) {
            return Err(CursorMappingError::DuplicateActionId(args.tool_call_id));
        }
        if self.seen_cooldex_call_ids.contains(&cooldex_call_id) {
            return Err(CursorMappingError::DuplicateCooldexCallId(cooldex_call_id));
        }

        let frozen_tool = self.snapshot.tool_for_identity(&args)?.clone();
        let arguments = prost_map_to_json_object(&args.args).map_err(|reason| {
            CursorMappingError::InvalidToolArguments {
                call_id: args.tool_call_id.clone(),
                reason,
            }
        })?;
        let response_item = match frozen_tool.kind {
            FrozenToolKind::Function => ResponseItem::FunctionCall {
                id: None,
                name: frozen_tool.source_name.name.clone(),
                namespace: frozen_tool.source_name.namespace.clone(),
                arguments: serde_json::to_string(&arguments).map_err(|error| {
                    CursorMappingError::InvalidToolArguments {
                        call_id: args.tool_call_id.clone(),
                        reason: error.to_string(),
                    }
                })?,
                call_id: cooldex_call_id.clone(),
                internal_chat_message_metadata_passthrough: None,
            },
            FrozenToolKind::ApplyPatch => ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: cooldex_call_id.clone(),
                name: APPLY_PATCH_TOOL_NAME.to_string(),
                namespace: None,
                input: extract_apply_patch_input(&arguments)?,
                internal_chat_message_metadata_passthrough: None,
            },
        };

        let cursor_action_id = args.tool_call_id;
        self.seen_action_ids.insert(cursor_action_id.clone());
        self.seen_cooldex_call_ids.insert(cooldex_call_id.clone());
        self.pending.insert(
            cursor_action_id.clone(),
            PendingLiveCall {
                exec_message_id,
                exec_id,
                cooldex_call_id: cooldex_call_id.clone(),
            },
        );
        Ok(AcceptedLiveToolCall {
            cursor_action_id,
            cooldex_call_id,
            tool_name: frozen_tool.source_name,
            response_item,
        })
    }

    pub fn complete_mcp_call(
        &mut self,
        cursor_action_id: &str,
        output: &FunctionCallOutputPayload,
    ) -> Result<CompletedLiveToolCall, CursorMappingError> {
        if self.completed_action_ids.contains(cursor_action_id) {
            return Err(CursorMappingError::DuplicateToolResult(
                cursor_action_id.to_string(),
            ));
        }
        if !self.pending.contains_key(cursor_action_id) {
            return Err(CursorMappingError::UnknownActionId(
                cursor_action_id.to_string(),
            ));
        }
        let mapped_output = map_tool_output(output)?;
        let Some(pending) = self.pending.remove(cursor_action_id) else {
            return Err(CursorMappingError::UnknownActionId(
                cursor_action_id.to_string(),
            ));
        };
        self.completed_action_ids
            .insert(cursor_action_id.to_string());
        Ok(CompletedLiveToolCall {
            cooldex_call_id: pending.cooldex_call_id,
            exec_client_message: ExecClientMessage {
                id: pending.exec_message_id,
                exec_id: pending.exec_id,
                local_execution_time_ms: None,
                message: Some(exec_client_message::Message::McpResult(mapped_output)),
            },
        })
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn require_no_pending(&self) -> Result<(), CursorMappingError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(CursorMappingError::PendingToolsAtTerminal(
                self.pending.len(),
            ))
        }
    }
}

fn validate_live_mcp_args(args: &McpArgs) -> Result<(), CursorMappingError> {
    if args.tool_call_id.is_empty() {
        return Err(CursorMappingError::EmptyActionId);
    }
    if args.smart_mode_approval.is_some() {
        return Err(CursorMappingError::SmartModeApprovalRequested);
    }
    if args.smart_mode_approval_only {
        return Err(CursorMappingError::ApprovalBypassRequested(
            "smart_mode_approval_only",
        ));
    }
    if args.skip_approval {
        return Err(CursorMappingError::ApprovalBypassRequested("skip_approval"));
    }
    if args.server_identifier != COOLDEX_MCP_SERVER_IDENTIFIER {
        return Err(CursorMappingError::InvalidServerIdentifier);
    }
    Ok(())
}
