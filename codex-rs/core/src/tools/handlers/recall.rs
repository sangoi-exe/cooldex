use std::collections::BTreeMap;

use codex_protocol::models::ResponseInputItem;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::context::ContextualUserFragment;
use crate::context::RecallContext;
use crate::function_tool::FunctionCallError;
use crate::session::recall::unavailable_recall_context_for_error;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;

const TOOL_NAME: &str = "recall";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallArgs {}

struct RecallToolOutput {
    context: RecallContext,
    code_mode_result: JsonValue,
}

impl ToolOutput for RecallToolOutput {
    fn log_preview(&self) -> String {
        format!("recall result ({} bytes)", self.context.json().len())
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        FunctionToolOutput::from_text(self.context.render(), Some(true))
            .to_response_item(call_id, payload)
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        self.code_mode_result.clone()
    }
}

pub struct RecallHandler;

impl ToolExecutor<ToolInvocation> for RecallHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: "Return a bounded chronological slice immediately before the latest surviving compaction in the current thread."
                .to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::new(),
                /*required*/ None,
                /*additional_properties*/ Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            let ToolInvocation {
                session,
                turn,
                payload,
                ..
            } = invocation;
            let arguments = match payload {
                ToolPayload::Function { arguments } => arguments,
                _ => {
                    return Err(FunctionCallError::RespondToModel(
                        "recall handler received unsupported payload".to_string(),
                    ));
                }
            };
            let _: RecallArgs = parse_arguments(arguments.as_str())?;
            let context = match session
                .load_current_thread_recall_context(turn.as_ref())
                .await
            {
                Ok(context) => context,
                Err(error) => unavailable_recall_context_for_error(session.thread_id, &error)
                    .map_err(|render_error| {
                        FunctionCallError::RespondToModel(format!(
                            "recall was unavailable and its bounded diagnostic could not be rendered: {render_error}"
                        ))
                    })?,
            };
            let code_mode_result = serde_json::from_str(context.json()).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "recall produced an invalid bounded result: {err}"
                ))
            })?;
            Ok(boxed_tool_output(RecallToolOutput {
                context,
                code_mode_result,
            }))
        })
    }
}

impl CoreToolRuntime for RecallHandler {}

#[cfg(test)]
#[path = "recall_tests.rs"]
mod tests;
