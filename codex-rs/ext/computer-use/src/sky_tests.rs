use std::path::PathBuf;
use std::time::Duration;

use image::DynamicImage;
use image::ImageBuffer;
use image::Rgb;
use image::codecs::jpeg::JpegEncoder;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::protocol::CLICK_TOOL_NAME;
use crate::protocol::ClickArgs;
use crate::protocol::ComputerUseRequest;
use crate::protocol::GET_SCREENSHOT_TOOL_NAME;
use crate::protocol::GetScreenshotArgs;
use crate::protocol::MouseButton;
use crate::protocol::SCREENSHOT_MIME_TYPE;
use crate::protocol::SKY_DEFAULT_MOUSE_SIZE_PX;
use crate::protocol::SKY_DEFAULT_POST_ACTION_SLEEP_MS;
use crate::protocol::SKY_DEFAULT_TIMEOUT_MS;
use crate::sky::parse_screenshot_stdout;
use crate::sky::screenshot_content_block;
use crate::sky::sky_invocation_for_request;
use crate::sky::validated_screenshot_payload_from_jpeg;

#[test]
fn sky_invocation_preserves_desktop_defaults_for_click() {
    let invocation = sky_invocation_for_request(
        PathBuf::from("/tmp/sky_linux_x64"),
        &ComputerUseRequest::Click(ClickArgs {
            x: 300,
            y: 400,
            click_count: Some(2),
            key: Some("Control_L+a".to_string()),
            mouse_button: Some(MouseButton::Middle),
        }),
    )
    .expect("click invocation");

    assert_eq!(invocation.binary_path, PathBuf::from("/tmp/sky_linux_x64"));
    assert_eq!(
        invocation.argv,
        vec![
            "--timeout-ms".to_string(),
            SKY_DEFAULT_TIMEOUT_MS.to_string(),
            "--mouse-size-px".to_string(),
            SKY_DEFAULT_MOUSE_SIZE_PX.to_string(),
            CLICK_TOOL_NAME.to_string(),
        ]
    );
    assert_eq!(
        invocation.stdin_json,
        json!({
            "x": 300,
            "y": 400,
            "click_count": 2,
            "key": "Control_L+a",
            "mouse_button": "middle",
        })
    );
    assert_eq!(
        invocation.post_action_sleep,
        Some(Duration::from_millis(SKY_DEFAULT_POST_ACTION_SLEEP_MS))
    );
}

#[test]
fn sky_invocation_uses_empty_json_for_get_screenshot() {
    let invocation = sky_invocation_for_request(
        PathBuf::from("/tmp/sky_linux_x64"),
        &ComputerUseRequest::GetScreenshot(GetScreenshotArgs {}),
    )
    .expect("screenshot invocation");

    assert_eq!(
        invocation.argv,
        vec![
            "--timeout-ms".to_string(),
            SKY_DEFAULT_TIMEOUT_MS.to_string(),
            "--mouse-size-px".to_string(),
            SKY_DEFAULT_MOUSE_SIZE_PX.to_string(),
            GET_SCREENSHOT_TOOL_NAME.to_string(),
        ]
    );
    assert_eq!(invocation.stdin_json, json!({}));
    assert_eq!(invocation.post_action_sleep, None);
}

#[test]
fn parse_screenshot_stdout_accepts_one_filepath() {
    let filepath = parse_screenshot_stdout(
        r#"
        [
          {
            "filepath": "./sky-output/screenshot.jpg",
            "data_url": "data:image/jpeg;base64,ignored"
          }
        ]
        "#,
    )
    .expect("parse screenshot stdout");

    assert_eq!(filepath, "./sky-output/screenshot.jpg");
}

#[test]
fn parse_screenshot_stdout_rejects_invalid_shapes() {
    let error = parse_screenshot_stdout(r#"[]"#).expect_err("empty list must fail");
    assert_eq!(error.code, crate::ComputerUseErrorCode::SkyProtocolError);

    let error =
        parse_screenshot_stdout(r#"[{"filepath": ""}]"#).expect_err("empty filepath must fail");
    assert_eq!(error.code, crate::ComputerUseErrorCode::SkyProtocolError);

    let error = parse_screenshot_stdout(r#"{"filepath":"bad"}"#)
        .expect_err("non-array screenshot stdout must fail");
    assert_eq!(error.code, crate::ComputerUseErrorCode::SkyProtocolError);
}

#[test]
fn validated_screenshot_payload_preserves_dimensions_and_original_detail_metadata() {
    let payload = validated_screenshot_payload_from_jpeg(&valid_jpeg(
        crate::SCREENSHOT_VIEWPORT_WIDTH,
        crate::SCREENSHOT_VIEWPORT_HEIGHT,
    ))
    .expect("valid screenshot payload");

    assert_eq!(payload.width, crate::SCREENSHOT_VIEWPORT_WIDTH);
    assert_eq!(payload.height, crate::SCREENSHOT_VIEWPORT_HEIGHT);
    assert_eq!(payload.mime_type, SCREENSHOT_MIME_TYPE);

    let image = screenshot_content_block(&payload);
    assert_eq!(image.mime_type, SCREENSHOT_MIME_TYPE);
    assert_eq!(
        image
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get(crate::IMAGE_DETAIL_META_KEY)),
        Some(&json!(crate::IMAGE_DETAIL_ORIGINAL))
    );
}

#[test]
fn validated_screenshot_payload_rejects_wrong_dimensions() {
    let error = validated_screenshot_payload_from_jpeg(&valid_jpeg(
        crate::SCREENSHOT_VIEWPORT_WIDTH - 1,
        crate::SCREENSHOT_VIEWPORT_HEIGHT,
    ))
    .expect_err("wrong dimensions must fail");

    assert_eq!(
        error.code,
        crate::ComputerUseErrorCode::ScreenshotDecodeFailed
    );
}

fn valid_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(width, height, Rgb([68, 85, 102])));
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, /*quality*/ 90);
    encoder.encode_image(&image).expect("encode jpeg");
    encoded
}
