//! Implements the MultiAgentV2 collaboration tool surface.
// Merge-safety anchor: the workspace-local production V2 collaboration surface is
// owned here; keep handler exports aligned with the spec/tool registration switch.

use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_task_path_target;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
pub(crate) use crate::tools::handlers::multi_agents_common::*;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::AgentPath;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::CollabAgentInteractionBeginEvent;
use codex_protocol::protocol::CollabAgentInteractionEndEvent;
use codex_protocol::protocol::CollabAgentInteractionTool;
use codex_protocol::protocol::CollabAgentSpawnBeginEvent;
use codex_protocol::protocol::CollabAgentSpawnEndEvent;
use codex_protocol::protocol::CollabCloseBeginEvent;
use codex_protocol::protocol::CollabCloseEndEvent;
use codex_protocol::protocol::CollabWaitingBeginEvent;
use codex_protocol::protocol::CollabWaitingEndEvent;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

#[cfg(test)]
pub(crate) use close_agent::CloseAgentResult;
pub(crate) use close_agent::Handler as CloseAgentHandler;
pub(crate) use followup_task::Handler as FollowupTaskHandler;
pub(crate) use list_agents::Handler as ListAgentsHandler;
pub(crate) use send_message::Handler as SendMessageHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

mod close_agent;
mod followup_task;
mod list_agents;
mod message_tool;
mod send_message;
mod spawn;
pub(crate) mod wait;
