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
use crate::session::recall::RecallIntervals;
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
struct RecallArgs {
    #[serde(default)]
    intervals: RecallIntervals,
}

struct RecallToolOutput {
    context: RecallContext,
    code_mode_result: JsonValue,
}

impl ToolOutput for RecallToolOutput {
    fn log_output(&self) -> String {
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
            description: "Return closed compaction intervals from the current thread as a chronological JSON list of intervals containing assistant messages and visible reasoning. The current open interval is excluded."
                .to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(
                    "intervals".to_string(),
                    JsonSchema::one_of(
                        vec![
                            JsonSchema::integer(/*description*/ None),
                            JsonSchema::string_enum(
                                vec![JsonValue::String("all".to_string())],
                                /*description*/ None,
                            ),
                        ],
                        Some("Closed compaction intervals to return: a positive integer for the newest count, or \"all\". Defaults to 1; requests larger than available clamp to available.".to_string()),
                    ),
                )]),
                /*required*/ None,
                /*additional_properties*/ Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolInvocation {
                session, payload, ..
            } = invocation;
            let arguments = match payload {
                ToolPayload::Function { arguments } => arguments,
                _ => {
                    return Err(FunctionCallError::RespondToModel(
                        "recall handler received unsupported payload".to_string(),
                    ));
                }
            };
            let args: RecallArgs = parse_arguments(arguments.as_str())?;
            let context = session
                .load_current_thread_recall_context(args.intervals)
                .await
                .map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to recall thread history: {error:#}"
                    ))
                })?;
            let code_mode_result = serde_json::from_str(context.json()).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "recall produced an invalid interval result: {err}"
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
