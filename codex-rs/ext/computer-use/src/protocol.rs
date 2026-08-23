use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

pub const COMPUTER_USE_SERVER_NAME: &str = "computer_use";

pub const SCREENSHOT_VIEWPORT_WIDTH: u32 = 1_440;
pub const SCREENSHOT_VIEWPORT_HEIGHT: u32 = 900;
pub const SCREENSHOT_VIEWPORT_DEPTH: u8 = 24;
pub const SCREENSHOT_VIEWPORT_DPI: u16 = 96;
pub const SCREENSHOT_VIEWPORT_SCALE: u8 = 1;
pub const SCREENSHOT_MIME_TYPE: &str = "image/jpeg";
pub const IMAGE_DETAIL_META_KEY: &str = "codex/imageDetail";
pub const IMAGE_DETAIL_ORIGINAL: &str = "original";

pub const SKY_DEFAULT_TIMEOUT_MS: u32 = 30_000;
pub const SKY_DEFAULT_MOUSE_SIZE_PX: u32 = 12;
pub const SKY_DEFAULT_POST_ACTION_SLEEP_MS: u64 = 100;

pub const START_TOOL_NAME: &str = "start";
pub const GET_ENVIRONMENT_TOOL_NAME: &str = "get_environment";
pub const GET_SCREENSHOT_TOOL_NAME: &str = "get_screenshot";
pub const CLICK_TOOL_NAME: &str = "click";
pub const DRAG_TOOL_NAME: &str = "drag";
pub const MOVE_TOOL_NAME: &str = "move";
pub const PRESS_KEY_TOOL_NAME: &str = "press_key";
pub const SCROLL_TOOL_NAME: &str = "scroll";
pub const TYPE_TEXT_TOOL_NAME: &str = "type_text";
pub const STOP_TOOL_NAME: &str = "stop";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct StartArgs {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct GetEnvironmentArgs {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct GetScreenshotArgs {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct StopArgs {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum MouseButton {
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    #[serde(rename = "middle")]
    Middle,
    #[serde(rename = "l")]
    L,
    #[serde(rename = "r")]
    R,
    #[serde(rename = "m")]
    M,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum ScrollDirection {
    #[serde(rename = "up")]
    Up,
    #[serde(rename = "down")]
    Down,
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    #[serde(rename = "u")]
    U,
    #[serde(rename = "d")]
    D,
    #[serde(rename = "l")]
    L,
    #[serde(rename = "r")]
    R,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClickArgs {
    #[serde(default)]
    #[schemars(
        description = "Optional positive click count. Omit to use the runtime default of one click."
    )]
    pub click_count: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Optional X11 keysym-style key or chord to hold while clicking.")]
    pub key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional mouse button. Omit to use the runtime default left button."
    )]
    pub mouse_button: Option<MouseButton>,
    #[schemars(
        description = "Required desktop X coordinate within the 1440 by 900 Computer Use display."
    )]
    pub x: i32,
    #[schemars(
        description = "Required desktop Y coordinate within the 1440 by 900 Computer Use display."
    )]
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DragArgs {
    #[schemars(
        description = "Required starting desktop X coordinate within the 1440 by 900 display."
    )]
    pub from_x: i32,
    #[schemars(
        description = "Required starting desktop Y coordinate within the 1440 by 900 display."
    )]
    pub from_y: i32,
    #[serde(default)]
    #[schemars(description = "Optional X11 keysym-style key or chord to hold during the drag.")]
    pub key: Option<String>,
    #[schemars(
        description = "Required destination desktop X coordinate within the 1440 by 900 display."
    )]
    pub to_x: i32,
    #[schemars(
        description = "Required destination desktop Y coordinate within the 1440 by 900 display."
    )]
    pub to_y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveArgs {
    #[serde(default)]
    #[schemars(
        description = "Optional X11 keysym-style key or chord to hold while moving the pointer."
    )]
    pub key: Option<String>,
    #[schemars(
        description = "Required destination desktop X coordinate within the 1440 by 900 display."
    )]
    pub x: i32,
    #[schemars(
        description = "Required destination desktop Y coordinate within the 1440 by 900 display."
    )]
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PressKeyArgs {
    #[schemars(
        description = "Required X11 keysym-style key or plus-separated chord, for example Return or Control_L+Shift_L+period."
    )]
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScrollArgs {
    #[schemars(description = "Required scroll direction.")]
    pub direction: ScrollDirection,
    #[serde(default)]
    #[schemars(description = "Optional X11 keysym-style key or chord to hold while scrolling.")]
    pub key: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional positive scroll distance in pixels.")]
    pub pixels: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "Optional desktop X coordinate. Supply both x and y together when targeting a specific origin point."
    )]
    pub x: Option<i32>,
    #[serde(default)]
    #[schemars(
        description = "Optional desktop Y coordinate. Supply both x and y together when targeting a specific origin point."
    )]
    pub y: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeTextArgs {
    #[schemars(description = "Required literal text to type into the current focus.")]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerUseRequest {
    Start(StartArgs),
    GetEnvironment(GetEnvironmentArgs),
    GetScreenshot(GetScreenshotArgs),
    Click(ClickArgs),
    Drag(DragArgs),
    Move(MoveArgs),
    PressKey(PressKeyArgs),
    Scroll(ScrollArgs),
    TypeText(TypeTextArgs),
    Stop(StopArgs),
}

impl ComputerUseRequest {
    pub const fn tool_name(&self) -> &'static str {
        match self {
            Self::Start(_) => START_TOOL_NAME,
            Self::GetEnvironment(_) => GET_ENVIRONMENT_TOOL_NAME,
            Self::GetScreenshot(_) => GET_SCREENSHOT_TOOL_NAME,
            Self::Click(_) => CLICK_TOOL_NAME,
            Self::Drag(_) => DRAG_TOOL_NAME,
            Self::Move(_) => MOVE_TOOL_NAME,
            Self::PressKey(_) => PRESS_KEY_TOOL_NAME,
            Self::Scroll(_) => SCROLL_TOOL_NAME,
            Self::TypeText(_) => TYPE_TEXT_TOOL_NAME,
            Self::Stop(_) => STOP_TOOL_NAME,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputOperation {
    Click,
    Drag,
    Move,
    PressKey,
    Scroll,
    TypeText,
}

impl InputOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Click => CLICK_TOOL_NAME,
            Self::Drag => DRAG_TOOL_NAME,
            Self::Move => MOVE_TOOL_NAME,
            Self::PressKey => PRESS_KEY_TOOL_NAME,
            Self::Scroll => SCROLL_TOOL_NAME,
            Self::TypeText => TYPE_TEXT_TOOL_NAME,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DesktopEnvironment {
    pub display: String,
    pub xauthority: String,
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub dpi: u16,
    pub scale: u8,
}

impl DesktopEnvironment {
    pub fn new(display: impl Into<String>, xauthority: impl Into<String>) -> Self {
        Self {
            display: display.into(),
            xauthority: xauthority.into(),
            width: SCREENSHOT_VIEWPORT_WIDTH,
            height: SCREENSHOT_VIEWPORT_HEIGHT,
            depth: SCREENSHOT_VIEWPORT_DEPTH,
            dpi: SCREENSHOT_VIEWPORT_DPI,
            scale: SCREENSHOT_VIEWPORT_SCALE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotPayload {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub mime_type: String,
}

impl ScreenshotPayload {
    pub fn new(bytes: Vec<u8>, width: u32, height: u32, mime_type: impl Into<String>) -> Self {
        Self {
            bytes,
            width,
            height,
            mime_type: mime_type.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerUseOutput {
    Running {
        session_id: String,
        environment: DesktopEnvironment,
    },
    Operation {
        session_id: String,
        operation: InputOperation,
    },
    Screenshot {
        session_id: String,
        screenshot: ScreenshotPayload,
    },
    Stopped {
        session_id: String,
    },
}

impl ComputerUseOutput {
    pub fn structured_content(&self) -> Value {
        match self {
            Self::Running {
                session_id,
                environment,
            } => serde_json::json!({
                "ok": true,
                "state": SessionState::Running,
                "session_id": session_id,
                "environment": environment,
            }),
            Self::Operation {
                session_id,
                operation,
            } => serde_json::json!({
                "ok": true,
                "state": SessionState::Running,
                "session_id": session_id,
                "operation": operation.as_str(),
            }),
            Self::Screenshot {
                session_id,
                screenshot,
            } => serde_json::json!({
                "ok": true,
                "state": SessionState::Running,
                "session_id": session_id,
                "width": screenshot.width,
                "height": screenshot.height,
                "mime_type": screenshot.mime_type,
            }),
            Self::Stopped { session_id } => serde_json::json!({
                "ok": true,
                "state": SessionState::Stopped,
                "session_id": session_id,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseErrorCode {
    SessionNotStarted,
    InvalidArgument,
    UnsupportedPlatform,
    RuntimeUnavailable,
    PrerequisiteMissing,
    SessionStartFailed,
    SessionUnhealthy,
    SkyTimeout,
    SkyCancelled,
    SkyFailed,
    SkyOutputTooLarge,
    SkyProtocolError,
    ScreenshotPathInvalid,
    ScreenshotFileInvalid,
    ScreenshotTooLarge,
    ScreenshotDecodeFailed,
    ScreenshotCleanupFailed,
    InternalError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerUseError {
    pub code: ComputerUseErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ComputerUseError {
    pub fn new(code: ComputerUseErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(
            ComputerUseErrorCode::InvalidArgument,
            message,
            /*retryable*/ false,
        )
    }

    pub fn sky_protocol_error(message: impl Into<String>) -> Self {
        Self::new(
            ComputerUseErrorCode::SkyProtocolError,
            message,
            /*retryable*/ false,
        )
    }

    pub fn screenshot_decode_failed(message: impl Into<String>) -> Self {
        Self::new(
            ComputerUseErrorCode::ScreenshotDecodeFailed,
            message,
            /*retryable*/ false,
        )
    }

    pub fn structured_content(&self) -> Value {
        serde_json::json!({
            "ok": false,
            "error": {
                "code": self.code,
                "message": self.message,
                "retryable": self.retryable,
            },
        })
    }
}
