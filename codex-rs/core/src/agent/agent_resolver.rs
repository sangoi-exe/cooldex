use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_protocol::ThreadId;
use std::sync::Arc;

// Merge-safety anchor: MultiAgentV2 target resolution stays task-path-only in this workspace; do not reintroduce raw thread-id fallback into the V2 surface.
/// Resolves a MultiAgentV2 tool-facing target using only canonical task-path syntax.
pub(crate) async fn resolve_agent_task_path_target(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    target: &str,
) -> Result<ThreadId, FunctionCallError> {
    register_session_root(session, turn);
    if ThreadId::from_string(target).is_ok() {
        return Err(FunctionCallError::RespondToModel(
            "MultiAgentV2 targets must use canonical task paths, not agent ids".to_string(),
        ));
    }

    session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, target)
        .await
        .map_err(|err| match err {
            codex_protocol::error::CodexErr::UnsupportedOperation(message) => {
                FunctionCallError::RespondToModel(message)
            }
            other => FunctionCallError::RespondToModel(other.to_string()),
        })
}

fn register_session_root(session: &Arc<Session>, turn: &Arc<TurnContext>) {
    session
        .services
        .agent_control
        .register_session_root(session.conversation_id, &turn.session_source);
}
