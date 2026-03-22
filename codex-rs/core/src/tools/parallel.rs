// Merge-safety anchor: parallel tool execution must preserve workspace-local tool-output
// routing and blocked/disallowed response contracts across mixed function/custom/MCP tools.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tokio_util::either::Either;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::instrument;
use tracing::trace_span;

use crate::client_common::tools::ToolSpec;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::error::CodexErr;
use crate::function_tool::FunctionCallError;
use crate::tools::context::AbortedToolOutput;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolPayload;
use crate::tools::registry::AnyToolResult;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use crate::tools::router::ToolRouter;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;

#[derive(Clone)]
pub(crate) struct ToolCallRuntime {
    router: Arc<ToolRouter>,
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    tracker: SharedTurnDiffTracker,
    parallel_execution: Arc<RwLock<()>>,
    allowed_tool_names: Option<HashSet<String>>,
    source: ToolCallSource,
}

impl ToolCallRuntime {
    pub(crate) fn new(
        router: Arc<ToolRouter>,
        session: Arc<Session>,
        turn_context: Arc<TurnContext>,
        tracker: SharedTurnDiffTracker,
        allowed_tool_names: Option<HashSet<String>>,
        source: ToolCallSource,
    ) -> Self {
        Self {
            router,
            session,
            turn_context,
            tracker,
            parallel_execution: Arc::new(RwLock::new(())),
            allowed_tool_names,
            source,
        }
    }

    pub(crate) fn find_spec(&self, tool_name: &str) -> Option<ToolSpec> {
        self.router.find_spec(tool_name)
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) async fn handle_tool_call(
        self,
        call: ToolCall,
        cancellation_token: CancellationToken,
    ) -> Result<ResponseInputItem, CodexErr> {
        let error_call = call.clone();
        if let Some(allowed_tool_names) = self.allowed_tool_names.as_ref()
            && !allowed_tool_names.contains(call.tool_name.as_str())
        {
            return Ok(Self::disallowed_response(&call));
        }
        let source = self.source;
        match self
            .handle_tool_call_with_source(call, source, cancellation_token)
            .await
        {
            Ok(response) => Ok(response.into_response()),
            Err(FunctionCallError::Fatal(message)) => Err(CodexErr::Fatal(message)),
            Err(other) => Ok(Self::failure_response(error_call, other)),
        }
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call_with_source(
        self,
        call: ToolCall,
        source: ToolCallSource,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<AnyToolResult, FunctionCallError>> {
        let supports_parallel = self.router.tool_supports_parallel(&call.tool_name);
        let router = Arc::clone(&self.router);
        let session = Arc::clone(&self.session);
        let turn = Arc::clone(&self.turn_context);
        let tracker = Arc::clone(&self.tracker);
        let lock = Arc::clone(&self.parallel_execution);
        let started = Instant::now();

        let dispatch_span = trace_span!(
            "dispatch_tool_call_with_code_mode_result",
            otel.name = call.tool_name.as_str(),
            tool_name = call.tool_name.as_str(),
            call_id = call.call_id.as_str(),
            aborted = false,
        );

        let handle: AbortOnDropHandle<Result<AnyToolResult, FunctionCallError>> =
            AbortOnDropHandle::new(tokio::spawn(async move {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        let secs = started.elapsed().as_secs_f32().max(0.1);
                        dispatch_span.record("aborted", true);
                        Ok(Self::aborted_response(&call, secs))
                    },
                    res = async {
                        let _guard = if supports_parallel {
                            Either::Left(lock.read().await)
                        } else {
                            Either::Right(lock.write().await)
                        };

                        router
                            .dispatch_tool_call_with_code_mode_result(
                                session,
                                turn,
                                tracker,
                                call.clone(),
                                source,
                            )
                            .instrument(dispatch_span.clone())
                            .await
                    } => res,
                }
            }));

        async move {
            handle.await.map_err(|err| {
                FunctionCallError::Fatal(format!("tool task failed to receive: {err:?}"))
            })?
        }
    }
}

impl ToolCallRuntime {
    fn failure_response(call: ToolCall, err: FunctionCallError) -> ResponseInputItem {
        let message = err.to_string();
        match call.payload {
            ToolPayload::ToolSearch { .. } => ResponseInputItem::ToolSearchOutput {
                call_id: call.call_id,
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: Vec::new(),
            },
            ToolPayload::Custom { .. } => ResponseInputItem::CustomToolCallOutput {
                call_id: call.call_id,
                name: None,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: codex_protocol::models::FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
            _ => ResponseInputItem::FunctionCallOutput {
                call_id: call.call_id,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: codex_protocol::models::FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
        }
    }

    fn aborted_response(call: &ToolCall, secs: f32) -> AnyToolResult {
        AnyToolResult {
            call_id: call.call_id.clone(),
            payload: call.payload.clone(),
            result: Box::new(AbortedToolOutput {
                message: Self::abort_message(call, secs),
            }),
        }
    }

    fn abort_message(call: &ToolCall, secs: f32) -> String {
        match call.tool_name.as_str() {
            "shell" | "container.exec" | "local_shell" | "shell_command" | "unified_exec" => {
                format!("Wall time: {secs:.1} seconds\naborted by user")
            }
            _ => format!("aborted by user after {secs:.1}s"),
        }
    }

    fn disallowed_response(call: &ToolCall) -> ResponseInputItem {
        let message = format!(
            "tool call blocked: {} is not allowed in this task",
            call.tool_name
        );
        match &call.payload {
            ToolPayload::Custom { .. } => ResponseInputItem::CustomToolCallOutput {
                call_id: call.call_id.clone(),
                name: Some(call.tool_name.clone()),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
            ToolPayload::Mcp { .. } => ResponseInputItem::McpToolCallOutput {
                call_id: call.call_id.clone(),
                output: CallToolResult::from_error_text(message),
            },
            _ => ResponseInputItem::FunctionCallOutput {
                call_id: call.call_id.clone(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn disallowed_response_sets_failed_success_for_function_and_custom_outputs() {
        let function_call = ToolCall {
            tool_name: "blocked_tool".to_string(),
            tool_namespace: None,
            call_id: "function-call".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
        };
        let custom_call = ToolCall {
            tool_name: "blocked_tool".to_string(),
            tool_namespace: None,
            call_id: "custom-call".to_string(),
            payload: ToolPayload::Custom {
                input: "{}".to_string(),
            },
        };

        let expected_message = "tool call blocked: blocked_tool is not allowed in this task";

        match ToolCallRuntime::disallowed_response(&function_call) {
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "function-call");
                assert_eq!(output.text_content(), Some(expected_message));
                assert_eq!(output.success, Some(false));
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }

        match ToolCallRuntime::disallowed_response(&custom_call) {
            ResponseInputItem::CustomToolCallOutput {
                call_id,
                name,
                output,
            } => {
                assert_eq!(call_id, "custom-call");
                assert_eq!(name.as_deref(), Some("blocked_tool"));
                assert_eq!(output.text_content(), Some(expected_message));
                assert_eq!(output.success, Some(false));
            }
            other => panic!("expected CustomToolCallOutput, got {other:?}"),
        }
    }
}
