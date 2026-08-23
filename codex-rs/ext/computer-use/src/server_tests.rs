use std::sync::Arc;
use std::sync::Mutex;

use pretty_assertions::assert_eq;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParams;
use rmcp::model::ToolAnnotations;
use serde_json::Value;
use serde_json::json;

use crate::ComputerUseServer;
use crate::computer_use_tools;
use crate::dispatch_tool_call;
use crate::protocol::CLICK_TOOL_NAME;
use crate::protocol::ClickArgs;
use crate::protocol::ComputerUseError;
use crate::protocol::ComputerUseOutput;
use crate::protocol::ComputerUseRequest;
use crate::protocol::DesktopEnvironment;
use crate::protocol::GET_ENVIRONMENT_TOOL_NAME;
use crate::protocol::GET_SCREENSHOT_TOOL_NAME;
use crate::protocol::GetScreenshotArgs;
use crate::protocol::InputOperation;
use crate::protocol::MOVE_TOOL_NAME;
use crate::protocol::PRESS_KEY_TOOL_NAME;
use crate::protocol::SCREENSHOT_MIME_TYPE;
use crate::protocol::SCROLL_TOOL_NAME;
use crate::protocol::START_TOOL_NAME;
use crate::protocol::STOP_TOOL_NAME;
use crate::protocol::TYPE_TEXT_TOOL_NAME;
use crate::sky::validated_screenshot_payload_from_jpeg;
use image::DynamicImage;
use image::ImageBuffer;
use image::Rgb;
use image::codecs::jpeg::JpegEncoder;

#[derive(Clone)]
struct FakeRuntime {
    response: Result<ComputerUseOutput, ComputerUseError>,
    requests: Arc<Mutex<Vec<ComputerUseRequest>>>,
}

impl FakeRuntime {
    fn success(response: ComputerUseOutput) -> Self {
        Self {
            response: Ok(response),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<ComputerUseRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl crate::ComputerUseRuntime for FakeRuntime {
    async fn execute(
        &self,
        request: ComputerUseRequest,
    ) -> Result<ComputerUseOutput, ComputerUseError> {
        self.requests.lock().expect("requests lock").push(request);
        self.response.clone()
    }
}

#[test]
fn tool_inventory_matches_contract() {
    let tools = computer_use_tools();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref().to_string())
        .collect::<Vec<_>>();
    let descriptions = tools
        .iter()
        .map(|tool| {
            tool.description
                .as_ref()
                .expect("tool description")
                .to_string()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            START_TOOL_NAME.to_string(),
            GET_ENVIRONMENT_TOOL_NAME.to_string(),
            GET_SCREENSHOT_TOOL_NAME.to_string(),
            CLICK_TOOL_NAME.to_string(),
            "drag".to_string(),
            MOVE_TOOL_NAME.to_string(),
            PRESS_KEY_TOOL_NAME.to_string(),
            SCROLL_TOOL_NAME.to_string(),
            TYPE_TEXT_TOOL_NAME.to_string(),
            STOP_TOOL_NAME.to_string(),
        ]
    );
    assert_eq!(
        descriptions,
        vec![
            "Start or reuse the owned Computer Use desktop session. Call this before get_environment, get_screenshot, or any input tool.".to_string(),
            "Return DISPLAY and XAUTHORITY for the running Computer Use session so shell or exec can launch GUI apps into this desktop.".to_string(),
            "Capture the current desktop as an original-detail JPEG. Use this to observe state before coordinate-based input and again after each mutating action.".to_string(),
            "Click one desktop coordinate in the Computer Use display, then refresh your observation before the next action.".to_string(),
            "Drag from one desktop coordinate to another in the Computer Use display, then refresh your observation before the next action.".to_string(),
            "Move the pointer to one desktop coordinate in the Computer Use display. Treat hover or focus changes as state changes and refresh before the next action.".to_string(),
            "Press one X11 keysym-style key or chord in the Computer Use display, then refresh your observation before the next action.".to_string(),
            "Scroll inside the Computer Use display by direction and optional coordinate, then refresh your observation before the next action.".to_string(),
            "Type literal text into the current focus in the Computer Use display. Verify focus first, then refresh your observation after typing.".to_string(),
            "Stop and clean up the owned Computer Use desktop session when you are done.".to_string(),
        ]
    );

    let expected_annotations = vec![
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(true)
            .open_world(false),
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(true)
            .open_world(false),
    ];

    let annotations = tools
        .iter()
        .map(|tool| tool.annotations.clone().expect("tool annotations"))
        .collect::<Vec<_>>();
    assert_eq!(annotations, expected_annotations);
}

#[test]
fn server_info_instructions_match_operating_sequence() {
    let runtime = FakeRuntime::success(ComputerUseOutput::Stopped {
        session_id: "session-seq".to_string(),
    });
    let server = ComputerUseServer::new(runtime);
    let info = server.get_info();

    assert_eq!(
        info.instructions.as_deref(),
        Some(
            "Use these tools to control the owned WSL/Linux Computer Use desktop. Call start first. Use get_environment before launching GUI apps through shell or exec into the returned DISPLAY and XAUTHORITY. Use get_screenshot to observe before coordinate-based input, perform one mutating action at a time, then observe again instead of reusing stale coordinates or focus assumptions. Call stop when you are done with the session."
        )
    );
}

#[test]
fn input_schemas_expose_local_contract_descriptions() {
    let tools = computer_use_tools();
    let click_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == CLICK_TOOL_NAME)
        .expect("click tool");
    let scroll_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == SCROLL_TOOL_NAME)
        .expect("scroll tool");
    let type_text_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == TYPE_TEXT_TOOL_NAME)
        .expect("type_text tool");

    assert_eq!(
        schema_property_description(click_tool.input_schema.as_ref(), "x"),
        Some("Required desktop X coordinate within the 1440 by 900 Computer Use display.")
    );
    assert_eq!(
        schema_property_description(click_tool.input_schema.as_ref(), "y"),
        Some("Required desktop Y coordinate within the 1440 by 900 Computer Use display.")
    );
    assert_eq!(
        schema_property_description(scroll_tool.input_schema.as_ref(), "pixels"),
        Some("Optional positive scroll distance in pixels.")
    );
    assert_eq!(
        schema_property_description(scroll_tool.input_schema.as_ref(), "x"),
        Some(
            "Optional desktop X coordinate. Supply both x and y together when targeting a specific origin point."
        )
    );
    assert_eq!(
        schema_property_description(type_text_tool.input_schema.as_ref(), "text"),
        Some("Required literal text to type into the current focus.")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_routes_click_to_runtime_and_returns_structured_operation() {
    let runtime = FakeRuntime::success(ComputerUseOutput::Operation {
        session_id: "session-123".to_string(),
        operation: InputOperation::Click,
    });

    let result = dispatch_tool_call(
        &runtime,
        CallToolRequestParams::new(CLICK_TOOL_NAME).with_arguments(arguments(json!({
            "x": 10,
            "y": 20,
            "click_count": 2,
            "mouse_button": "right",
            "key": "Shift+Tab",
        }))),
    )
    .await
    .expect("dispatch click");

    assert_eq!(
        runtime.requests(),
        vec![ComputerUseRequest::Click(ClickArgs {
            x: 10,
            y: 20,
            click_count: Some(2),
            mouse_button: Some(crate::protocol::MouseButton::Right),
            key: Some("Shift+Tab".to_string()),
        })]
    );
    assert!(result.content.is_empty());
    assert_eq!(
        result.structured_content,
        Some(json!({
            "ok": true,
            "state": "running",
            "session_id": "session-123",
            "operation": CLICK_TOOL_NAME,
        }))
    );
    assert_eq!(result.is_error, Some(false));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_returns_original_detail_image_for_screenshot() {
    let screenshot_payload = validated_screenshot_payload_from_jpeg(&valid_jpeg(
        crate::SCREENSHOT_VIEWPORT_WIDTH,
        crate::SCREENSHOT_VIEWPORT_HEIGHT,
    ))
    .expect("valid screenshot payload");
    let runtime = FakeRuntime::success(ComputerUseOutput::Screenshot {
        session_id: "session-456".to_string(),
        screenshot: screenshot_payload.clone(),
    });

    let result = dispatch_tool_call(
        &runtime,
        CallToolRequestParams::new(GET_SCREENSHOT_TOOL_NAME).with_arguments(arguments(json!({}))),
    )
    .await
    .expect("dispatch screenshot");

    assert_eq!(
        runtime.requests(),
        vec![ComputerUseRequest::GetScreenshot(GetScreenshotArgs {})]
    );
    assert_eq!(
        result.structured_content,
        Some(json!({
            "ok": true,
            "state": "running",
            "session_id": "session-456",
            "width": crate::SCREENSHOT_VIEWPORT_WIDTH,
            "height": crate::SCREENSHOT_VIEWPORT_HEIGHT,
            "mime_type": SCREENSHOT_MIME_TYPE,
        }))
    );
    assert_eq!(result.is_error, Some(false));
    assert_eq!(result.content.len(), 1);
    let image = result.content[0].as_image().expect("image content");
    assert_eq!(image.mime_type, SCREENSHOT_MIME_TYPE);
    assert_eq!(
        image
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get(crate::IMAGE_DETAIL_META_KEY)),
        Some(&json!(crate::IMAGE_DETAIL_ORIGINAL))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_maps_parse_failures_to_invalid_argument_without_runtime_call() {
    let runtime = FakeRuntime::success(ComputerUseOutput::Running {
        session_id: "session-789".to_string(),
        environment: DesktopEnvironment::new(":99", "/tmp/.Xauthority"),
    });

    let result = dispatch_tool_call(
        &runtime,
        CallToolRequestParams::new(PRESS_KEY_TOOL_NAME).with_arguments(arguments(json!({}))),
    )
    .await
    .expect("dispatch parse failure");

    assert!(runtime.requests().is_empty());
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content,
        Some(json!({
            "ok": false,
            "error": {
                "code": "invalid_argument",
                "message": format!("invalid arguments for {PRESS_KEY_TOOL_NAME}: missing field `key`"),
                "retryable": false,
            }
        }))
    );
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().expect("object arguments").clone()
}

fn schema_property_description<'a>(
    schema: &'a serde_json::Map<String, Value>,
    property: &str,
) -> Option<&'a str> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(property))
        .and_then(Value::as_object)
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
}

fn valid_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(width, height, Rgb([17, 34, 51])));
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, /*quality*/ 90);
    encoder.encode_image(&image).expect("encode jpeg");
    encoded
}
