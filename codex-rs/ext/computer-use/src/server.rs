use std::future::Future;
use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::Tool;
use rmcp::model::ToolAnnotations;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::protocol::CLICK_TOOL_NAME;
use crate::protocol::ClickArgs;
use crate::protocol::ComputerUseError;
use crate::protocol::ComputerUseOutput;
use crate::protocol::ComputerUseRequest;
use crate::protocol::DRAG_TOOL_NAME;
use crate::protocol::DragArgs;
use crate::protocol::GET_ENVIRONMENT_TOOL_NAME;
use crate::protocol::GET_SCREENSHOT_TOOL_NAME;
use crate::protocol::GetEnvironmentArgs;
use crate::protocol::GetScreenshotArgs;
use crate::protocol::MOVE_TOOL_NAME;
use crate::protocol::MoveArgs;
use crate::protocol::PRESS_KEY_TOOL_NAME;
use crate::protocol::PressKeyArgs;
use crate::protocol::SCROLL_TOOL_NAME;
use crate::protocol::START_TOOL_NAME;
use crate::protocol::STOP_TOOL_NAME;
use crate::protocol::ScrollArgs;
use crate::protocol::StartArgs;
use crate::protocol::StopArgs;
use crate::protocol::TYPE_TEXT_TOOL_NAME;
use crate::protocol::TypeTextArgs;
use crate::sky::screenshot_content_block;

const COMPUTER_USE_SERVER_INSTRUCTIONS: &str = "Use these tools to control the owned WSL/Linux Computer Use desktop. Call start first. \
Use get_environment before launching GUI apps through shell or exec into the returned DISPLAY \
and XAUTHORITY. Use get_screenshot to observe before coordinate-based input, perform one \
mutating action at a time, then observe again instead of reusing stale coordinates or focus \
assumptions. Call stop when you are done with the session.";
const START_TOOL_DESCRIPTION: &str = "Start or reuse the owned Computer Use desktop session. Call this before get_environment, \
get_screenshot, or any input tool.";
const GET_ENVIRONMENT_TOOL_DESCRIPTION: &str = "Return DISPLAY and XAUTHORITY for the running Computer Use session so shell or exec can \
launch GUI apps into this desktop.";
const GET_SCREENSHOT_TOOL_DESCRIPTION: &str = "Capture the current desktop as an original-detail JPEG. Use this to observe state before \
coordinate-based input and again after each mutating action.";
const CLICK_TOOL_DESCRIPTION: &str = "Click one desktop coordinate in the Computer Use display, then refresh your observation \
before the next action.";
const DRAG_TOOL_DESCRIPTION: &str = "Drag from one desktop coordinate to another in the Computer Use display, then refresh your \
observation before the next action.";
const MOVE_TOOL_DESCRIPTION: &str = "Move the pointer to one desktop coordinate in the Computer Use display. Treat hover or \
focus changes as state changes and refresh before the next action.";
const PRESS_KEY_TOOL_DESCRIPTION: &str = "Press one X11 keysym-style key or chord in the Computer Use display, then refresh your \
observation before the next action.";
const SCROLL_TOOL_DESCRIPTION: &str = "Scroll inside the Computer Use display by direction and optional coordinate, then refresh \
your observation before the next action.";
const TYPE_TEXT_TOOL_DESCRIPTION: &str = "Type literal text into the current focus in the Computer Use display. Verify focus first, \
then refresh your observation after typing.";
const STOP_TOOL_DESCRIPTION: &str =
    "Stop and clean up the owned Computer Use desktop session when you are done.";

/// Runtime owner for the Computer Use MCP surface.
///
/// Implementations receive already-parsed tool requests and return stable
/// domain outputs or stable domain failures. They must not depend on RMCP
/// request parsing details.
pub trait ComputerUseRuntime: Send + Sync + 'static {
    fn execute(
        &self,
        request: ComputerUseRequest,
    ) -> impl Future<Output = Result<ComputerUseOutput, ComputerUseError>> + Send;
}

#[derive(Clone)]
pub struct ComputerUseServer<Runtime> {
    runtime: Runtime,
    tools: Arc<Vec<Tool>>,
}

impl<Runtime> ComputerUseServer<Runtime> {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            tools: Arc::new(computer_use_tools()),
        }
    }
}

impl<Runtime> ComputerUseServer<Runtime>
where
    Runtime: ComputerUseRuntime,
{
    pub async fn dispatch(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        dispatch_tool_call(&self.runtime, request).await
    }
}

impl<Runtime> ServerHandler for ComputerUseServer<Runtime>
where
    Runtime: ComputerUseRuntime,
{
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(COMPUTER_USE_SERVER_INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        let tools = self.tools.clone();
        async move { Ok(ListToolsResult::with_all_items((*tools).clone())) }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        Ok(self.dispatch(request).await?.into())
    }
}

pub fn computer_use_tools() -> Vec<Tool> {
    vec![
        tool_definition::<StartArgs>(
            START_TOOL_NAME,
            START_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(true)
                .open_world(false),
        ),
        tool_definition::<GetEnvironmentArgs>(
            GET_ENVIRONMENT_TOOL_NAME,
            GET_ENVIRONMENT_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        tool_definition::<GetScreenshotArgs>(
            GET_SCREENSHOT_TOOL_NAME,
            GET_SCREENSHOT_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        tool_definition::<ClickArgs>(
            CLICK_TOOL_NAME,
            CLICK_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        ),
        tool_definition::<DragArgs>(
            DRAG_TOOL_NAME,
            DRAG_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        ),
        tool_definition::<MoveArgs>(
            MOVE_TOOL_NAME,
            MOVE_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        ),
        tool_definition::<PressKeyArgs>(
            PRESS_KEY_TOOL_NAME,
            PRESS_KEY_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        ),
        tool_definition::<ScrollArgs>(
            SCROLL_TOOL_NAME,
            SCROLL_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        ),
        tool_definition::<TypeTextArgs>(
            TYPE_TEXT_TOOL_NAME,
            TYPE_TEXT_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        ),
        tool_definition::<StopArgs>(
            STOP_TOOL_NAME,
            STOP_TOOL_DESCRIPTION,
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(true)
                .open_world(false),
        ),
    ]
}

pub async fn dispatch_tool_call<Runtime>(
    runtime: &Runtime,
    request: CallToolRequestParams,
) -> Result<CallToolResult, ErrorData>
where
    Runtime: ComputerUseRuntime,
{
    let parsed_request = match parse_call_tool_request(request) {
        Ok(parsed_request) => parsed_request,
        Err(ParseToolRequestError::UnknownTool(name)) => {
            return Err(ErrorData::invalid_params(
                format!("unknown tool: {name}"),
                None,
            ));
        }
        Err(ParseToolRequestError::InvalidArguments(error)) => {
            return Ok(error_call_tool_result(error));
        }
    };

    match runtime.execute(parsed_request).await {
        Ok(output) => success_call_tool_result(output),
        Err(error) => Ok(error_call_tool_result(error)),
    }
}

enum ParseToolRequestError {
    UnknownTool(String),
    InvalidArguments(ComputerUseError),
}

fn parse_call_tool_request(
    request: CallToolRequestParams,
) -> Result<ComputerUseRequest, ParseToolRequestError> {
    let tool_name = request.name.to_string();
    let arguments = request.arguments.unwrap_or_default();

    match tool_name.as_str() {
        START_TOOL_NAME => {
            parse_tool_args::<StartArgs>(arguments, &tool_name).map(ComputerUseRequest::Start)
        }
        GET_ENVIRONMENT_TOOL_NAME => parse_tool_args::<GetEnvironmentArgs>(arguments, &tool_name)
            .map(ComputerUseRequest::GetEnvironment),
        GET_SCREENSHOT_TOOL_NAME => parse_tool_args::<GetScreenshotArgs>(arguments, &tool_name)
            .map(ComputerUseRequest::GetScreenshot),
        CLICK_TOOL_NAME => {
            parse_tool_args::<ClickArgs>(arguments, &tool_name).map(ComputerUseRequest::Click)
        }
        DRAG_TOOL_NAME => {
            parse_tool_args::<DragArgs>(arguments, &tool_name).map(ComputerUseRequest::Drag)
        }
        MOVE_TOOL_NAME => {
            parse_tool_args::<MoveArgs>(arguments, &tool_name).map(ComputerUseRequest::Move)
        }
        PRESS_KEY_TOOL_NAME => {
            parse_tool_args::<PressKeyArgs>(arguments, &tool_name).map(ComputerUseRequest::PressKey)
        }
        SCROLL_TOOL_NAME => {
            parse_tool_args::<ScrollArgs>(arguments, &tool_name).map(ComputerUseRequest::Scroll)
        }
        TYPE_TEXT_TOOL_NAME => {
            parse_tool_args::<TypeTextArgs>(arguments, &tool_name).map(ComputerUseRequest::TypeText)
        }
        STOP_TOOL_NAME => {
            parse_tool_args::<StopArgs>(arguments, &tool_name).map(ComputerUseRequest::Stop)
        }
        _ => Err(ParseToolRequestError::UnknownTool(tool_name)),
    }
}

fn parse_tool_args<Arguments>(
    arguments: JsonObject,
    tool_name: &str,
) -> Result<Arguments, ParseToolRequestError>
where
    Arguments: DeserializeOwned,
{
    serde_json::from_value(Value::Object(arguments)).map_err(|error| {
        ParseToolRequestError::InvalidArguments(ComputerUseError::invalid_argument(format!(
            "invalid arguments for {tool_name}: {error}",
        )))
    })
}

fn tool_definition<Arguments>(
    name: &'static str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Tool
where
    Arguments: schemars::JsonSchema + 'static,
{
    Tool::new(name, description, Arc::new(JsonObject::new()))
        .with_input_schema::<Arguments>()
        .with_annotations(annotations)
}

fn success_call_tool_result(output: ComputerUseOutput) -> Result<CallToolResult, ErrorData> {
    let content = match &output {
        ComputerUseOutput::Screenshot { screenshot, .. } => {
            vec![ContentBlock::Image(screenshot_content_block(screenshot))]
        }
        ComputerUseOutput::Running { .. }
        | ComputerUseOutput::Operation { .. }
        | ComputerUseOutput::Stopped { .. } => Vec::new(),
    };

    let mut result = CallToolResult::success(content);
    result.structured_content = Some(output.structured_content());
    Ok(result)
}

fn error_call_tool_result(error: ComputerUseError) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(error.message.clone())]);
    result.structured_content = Some(error.structured_content());
    result
}
