// Merge-safety anchor: MultiAgentV2 agent listing must stay aligned with the
// workspace-local live-agent registry projection and list_agents tool schema.

use super::*;
use crate::agent::control::ListedAgent;
use crate::tools::handlers::multi_agents_common::collab_spawn_error;

pub(crate) struct Handler;

#[async_trait]
impl ToolHandler for Handler {
    type Output = ListAgentsResult;

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
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let args: ListAgentsArgs = parse_arguments(&arguments)?;
        session
            .services
            .agent_control
            .register_session_root(session.conversation_id, &turn.session_source);
        let agents = session
            .services
            .agent_control
            .list_agents(&turn.session_source, args.path_prefix.as_deref())
            .await
            .map_err(collab_spawn_error)?;

        Ok(ListAgentsResult { agents })
    }
}

#[derive(Debug, Deserialize)]
struct ListAgentsArgs {
    path_prefix: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ListAgentsResult {
    agents: Vec<ListedAgent>,
}

impl ToolOutput for ListAgentsResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "list_agents")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "list_agents")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "list_agents")
    }
}
