use std::collections::BTreeMap;
use std::fmt;

use codex_protocol::models::ResponseInputItem;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde::de::MapAccess;
use serde::de::Visitor;
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

#[derive(Debug, Eq, PartialEq)]
struct RecallArgs {
    intervals: RecallIntervals,
}

impl<'de> Deserialize<'de> for RecallArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RecallArgsVisitor;

        impl<'de> Visitor<'de> for RecallArgsVisitor {
            type Value = RecallArgs;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with an optional intervals property")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut intervals = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "intervals" => {
                            if intervals.is_some() {
                                return Err(A::Error::duplicate_field("intervals"));
                            }
                            intervals = Some(map.next_value()?);
                        }
                        _ => return Err(A::Error::unknown_field(&key, &["intervals"])),
                    }
                }
                Ok(RecallArgs {
                    intervals: intervals.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_map(RecallArgsVisitor)
    }
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
