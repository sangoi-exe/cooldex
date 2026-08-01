mod history;
mod live_tools;
mod request;
mod tools;
mod values;

pub use live_tools::AcceptedLiveToolCall;
pub use live_tools::CompletedLiveToolCall;
pub use live_tools::CursorToolCallTracker;
pub use request::CursorSamplingRequest;
pub use request::MappedCursorRunRequest;
pub use request::build_request_context;
pub use request::map_interaction_update;
pub use request::map_request_context_result;
pub use request::map_sampling_request;
use thiserror::Error;
pub use tools::CursorToolSnapshot;

pub const COOLDEX_MCP_SERVER_IDENTIFIER: &str = "cooldex";
pub const COOLDEX_BASE_INSTRUCTIONS_RULE_PATH: &str = "cooldex://base-instructions";

pub(super) const APPLY_PATCH_TOOL_NAME: &str = "apply_patch";
pub(super) const MAX_TOOL_NAME_BYTES: usize = 256;
pub(super) const MAX_TOOL_DESCRIPTION_BYTES: usize = 16_384;
pub(super) const MAX_TOOL_SCHEMA_BYTES: usize = 1024 * 1024;
pub(super) const MAX_TOTAL_TOOL_SCHEMA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CursorMappingError {
    #[error("Cursor AgentService tool name is empty")]
    EmptyToolName,
    #[error("Cursor AgentService tool name exceeds {MAX_TOOL_NAME_BYTES} bytes: {0}")]
    ToolNameTooLong(String),
    #[error("Cursor AgentService tool description exceeds {MAX_TOOL_DESCRIPTION_BYTES} bytes: {0}")]
    ToolDescriptionTooLong(String),
    #[error("Cursor AgentService tool schema exceeds {MAX_TOOL_SCHEMA_BYTES} bytes: {0}")]
    ToolSchemaTooLarge(String),
    #[error("Cursor AgentService total tool schema exceeds {MAX_TOTAL_TOOL_SCHEMA_BYTES} bytes")]
    TotalToolSchemaTooLarge,
    #[error("Cursor AgentService cannot represent tool {0}")]
    UnsupportedTool(String),
    #[error("Cursor AgentService cannot represent deferred tool {0}")]
    DeferredTool(String),
    #[error("Cursor AgentService cannot represent output schema for tool {0}")]
    UnsupportedOutputSchema(String),
    #[error("Cursor AgentService tool schema is invalid for {tool}: {reason}")]
    InvalidToolSchema { tool: String, reason: String },
    #[error("duplicate Cursor AgentService MCP identity {provider_identifier}/{tool_name}")]
    DuplicateToolIdentity {
        provider_identifier: String,
        tool_name: String,
    },
    #[error("duplicate Cursor AgentService wire tool name {0}")]
    DuplicateWireToolName(String),
    #[error("Cursor AgentService request requires a current or synthesized user message")]
    MissingCurrentUserMessage,
    #[error("Cursor AgentService current user message id is empty")]
    EmptyCurrentMessageId,
    #[error("Cursor AgentService cannot represent message role {0}")]
    UnsupportedMessageRole(String),
    #[error("Cursor AgentService cannot represent {0} message content")]
    UnsupportedMessageContent(String),
    #[error("Cursor AgentService history item appears before a user message: {0}")]
    HistoryItemBeforeUser(String),
    #[error("Cursor AgentService cannot represent history item {0}")]
    UnsupportedHistoryItem(String),
    #[error("duplicate historical tool call id {0}")]
    DuplicateHistoricalCallId(String),
    #[error("historical tool output has no matching call: {0}")]
    HistoricalOutputBeforeCall(String),
    #[error("historical tool call has no output: {0}")]
    IncompleteHistoricalCall(String),
    #[error("historical tool call has more than one output: {0}")]
    DuplicateHistoricalOutput(String),
    #[error("historical tool output kind does not match call: {0}")]
    HistoricalOutputKindMismatch(String),
    #[error("historical custom tool shape is not canonical apply_patch: {0}")]
    NonCanonicalCustomTool(String),
    #[error("tool arguments are not a JSON object for call {call_id}: {reason}")]
    InvalidToolArguments { call_id: String, reason: String },
    #[error("apply_patch arguments must contain exactly one string field named patch")]
    InvalidApplyPatchArguments,
    #[error("Cursor AgentService requested SmartModeApproval")]
    SmartModeApprovalRequested,
    #[error("Cursor AgentService requested approval bypass through {0}")]
    ApprovalBypassRequested(&'static str),
    #[error("Cursor AgentService MCP server identifier is not cooldex")]
    InvalidServerIdentifier,
    #[error("Cursor AgentService MCP identity does not match the frozen tool snapshot")]
    InvalidToolIdentity,
    #[error("Cursor AgentService MCP action id is empty")]
    EmptyActionId,
    #[error("duplicate Cursor AgentService MCP action id {0}")]
    DuplicateActionId(String),
    #[error("duplicate Cooldex tool call id {0}")]
    DuplicateCooldexCallId(String),
    #[error("Cursor AgentService exceeded the pending tool action limit {0}")]
    PendingToolLimit(usize),
    #[error("Cursor AgentService result references unknown action id {0}")]
    UnknownActionId(String),
    #[error("Cursor AgentService action already received a result: {0}")]
    DuplicateToolResult(String),
    #[error("Cursor AgentService terminal arrived with {0} pending tool actions")]
    PendingToolsAtTerminal(usize),
    #[error("Cursor AgentService cannot represent tool output content")]
    UnsupportedToolOutput,
    #[error("Cursor AgentService request-context action contains workspace state")]
    WorkspaceContextRequested,
    #[error("Cursor AgentService interaction update is unsupported")]
    UnsupportedInteractionUpdate,
}
