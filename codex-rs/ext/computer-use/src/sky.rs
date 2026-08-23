use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::GenericImageView;
use image::ImageFormat;
use image::ImageReader;
use image::Limits;
use rmcp::model::ImageContent;
use rmcp::model::MetaObject;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::protocol::COMPUTER_USE_SERVER_NAME;
use crate::protocol::ComputerUseError;
use crate::protocol::ComputerUseRequest;
use crate::protocol::GET_SCREENSHOT_TOOL_NAME;
use crate::protocol::IMAGE_DETAIL_META_KEY;
use crate::protocol::IMAGE_DETAIL_ORIGINAL;
use crate::protocol::SCREENSHOT_MIME_TYPE;
use crate::protocol::SCREENSHOT_VIEWPORT_HEIGHT;
use crate::protocol::SCREENSHOT_VIEWPORT_WIDTH;
use crate::protocol::SKY_DEFAULT_MOUSE_SIZE_PX;
use crate::protocol::SKY_DEFAULT_POST_ACTION_SLEEP_MS;
use crate::protocol::SKY_DEFAULT_TIMEOUT_MS;
use crate::protocol::ScreenshotPayload;

const SCREENSHOT_DECODE_MAX_ALLOC_BYTES: u64 = 16 * 1024 * 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyInvocation {
    pub binary_path: PathBuf,
    pub argv: Vec<String>,
    pub stdin_json: Value,
    pub post_action_sleep: Option<Duration>,
}

pub fn sky_invocation_for_request(
    binary_path: PathBuf,
    request: &ComputerUseRequest,
) -> Option<SkyInvocation> {
    match request {
        ComputerUseRequest::Start(_)
        | ComputerUseRequest::GetEnvironment(_)
        | ComputerUseRequest::Stop(_) => None,
        ComputerUseRequest::GetScreenshot(_) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(GET_SCREENSHOT_TOOL_NAME),
            stdin_json: json!({}),
            post_action_sleep: None,
        }),
        ComputerUseRequest::Click(arguments) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(request.tool_name()),
            stdin_json: json!(arguments),
            post_action_sleep: Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS)),
        }),
        ComputerUseRequest::Drag(arguments) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(request.tool_name()),
            stdin_json: json!(arguments),
            post_action_sleep: Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS)),
        }),
        ComputerUseRequest::Move(arguments) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(request.tool_name()),
            stdin_json: json!(arguments),
            post_action_sleep: Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS)),
        }),
        ComputerUseRequest::PressKey(arguments) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(request.tool_name()),
            stdin_json: json!(arguments),
            post_action_sleep: Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS)),
        }),
        ComputerUseRequest::Scroll(arguments) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(request.tool_name()),
            stdin_json: json!(arguments),
            post_action_sleep: Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS)),
        }),
        ComputerUseRequest::TypeText(arguments) => Some(SkyInvocation {
            binary_path,
            argv: sky_argv(request.tool_name()),
            stdin_json: json!(arguments),
            post_action_sleep: Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS)),
        }),
    }
}

pub fn parse_screenshot_stdout(stdout: &str) -> Result<String, ComputerUseError> {
    #[derive(Deserialize)]
    struct ScreenshotEntry {
        filepath: String,
    }

    let entries: Vec<ScreenshotEntry> = serde_json::from_str(stdout).map_err(|error| {
        ComputerUseError::sky_protocol_error(format!(
            "{COMPUTER_USE_SERVER_NAME} expected screenshot stdout JSON: {error}",
        ))
    })?;

    match entries.as_slice() {
        [entry] if !entry.filepath.is_empty() => Ok(entry.filepath.clone()),
        [entry] => Err(ComputerUseError::sky_protocol_error(format!(
            "{COMPUTER_USE_SERVER_NAME} received an empty screenshot filepath: {:?}",
            entry.filepath
        ))),
        _ => Err(ComputerUseError::sky_protocol_error(format!(
            "{COMPUTER_USE_SERVER_NAME} expected exactly one screenshot filepath in Sky stdout",
        ))),
    }
}

pub fn validated_screenshot_payload_from_jpeg(
    bytes: &[u8],
) -> Result<ScreenshotPayload, ComputerUseError> {
    let mut dimensions_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| {
            ComputerUseError::screenshot_decode_failed(format!(
                "failed to detect screenshot format: {error}",
            ))
        })?;
    if dimensions_reader.format() != Some(ImageFormat::Jpeg) {
        return Err(ComputerUseError::screenshot_decode_failed(
            "expected a JPEG screenshot payload",
        ));
    }

    dimensions_reader.limits(decode_limits());
    let (width, height) = dimensions_reader.into_dimensions().map_err(|error| {
        ComputerUseError::screenshot_decode_failed(format!(
            "failed to read screenshot dimensions: {error}",
        ))
    })?;
    if width != SCREENSHOT_VIEWPORT_WIDTH || height != SCREENSHOT_VIEWPORT_HEIGHT {
        return Err(ComputerUseError::screenshot_decode_failed(format!(
            "expected screenshot dimensions {SCREENSHOT_VIEWPORT_WIDTH}x{SCREENSHOT_VIEWPORT_HEIGHT}, got {width}x{height}"
        )));
    }

    let mut decode_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| {
            ComputerUseError::screenshot_decode_failed(format!(
                "failed to reopen screenshot payload: {error}",
            ))
        })?;
    decode_reader.limits(decode_limits());

    let decoded_image = decode_reader.decode().map_err(|error| {
        ComputerUseError::screenshot_decode_failed(format!(
            "failed to decode screenshot payload: {error}",
        ))
    })?;
    let (decoded_width, decoded_height) = decoded_image.dimensions();
    if decoded_width != SCREENSHOT_VIEWPORT_WIDTH || decoded_height != SCREENSHOT_VIEWPORT_HEIGHT {
        return Err(ComputerUseError::screenshot_decode_failed(format!(
            "expected decoded screenshot dimensions {SCREENSHOT_VIEWPORT_WIDTH}x{SCREENSHOT_VIEWPORT_HEIGHT}, got {decoded_width}x{decoded_height}"
        )));
    }

    Ok(ScreenshotPayload::new(
        bytes.to_vec(),
        width,
        height,
        SCREENSHOT_MIME_TYPE,
    ))
}

pub fn screenshot_content_block(payload: &ScreenshotPayload) -> ImageContent {
    let mut meta = MetaObject::new();
    meta.0.insert(
        IMAGE_DETAIL_META_KEY.to_string(),
        Value::String(IMAGE_DETAIL_ORIGINAL.to_string()),
    );

    ImageContent::new(
        BASE64_STANDARD.encode(&payload.bytes),
        payload.mime_type.clone(),
    )
    .with_meta(meta)
}

fn sky_argv(command_name: &str) -> Vec<String> {
    vec![
        "--timeout-ms".to_string(),
        SKY_DEFAULT_TIMEOUT_MS.to_string(),
        "--mouse-size-px".to_string(),
        SKY_DEFAULT_MOUSE_SIZE_PX.to_string(),
        command_name.to_string(),
    ]
}

fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(SCREENSHOT_VIEWPORT_WIDTH);
    limits.max_image_height = Some(SCREENSHOT_VIEWPORT_HEIGHT);
    limits.max_alloc = Some(SCREENSHOT_DECODE_MAX_ALLOC_BYTES);
    limits
}
