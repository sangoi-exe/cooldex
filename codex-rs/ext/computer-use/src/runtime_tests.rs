#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use image::DynamicImage;
use image::ImageBuffer;
use image::Rgb;
use image::codecs::jpeg::JpegEncoder;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::ComputerUseOutput;
use crate::ComputerUseRequest;
use crate::ComputerUseRuntime as _;
use crate::DesktopCommandPaths;
use crate::LocalComputerUseRuntime;
use crate::LocalComputerUseRuntimeConfig;
use crate::SCREENSHOT_VIEWPORT_HEIGHT;
use crate::SCREENSHOT_VIEWPORT_WIDTH;
use crate::protocol::GetEnvironmentArgs;
use crate::protocol::GetScreenshotArgs;
use crate::protocol::InputOperation;
use crate::protocol::StartArgs;
use crate::protocol::StopArgs;
use crate::protocol::TypeTextArgs;
use crate::server::RequestCancellation;

#[tokio::test(flavor = "current_thread")]
async fn runtime_requires_start_before_get_environment() {
    let harness = RuntimeHarness::with_click_sky();
    let runtime = harness.runtime();

    let error = runtime
        .execute(ComputerUseRequest::GetEnvironment(GetEnvironmentArgs {}))
        .await
        .expect_err("get_environment must fail before start");

    assert_eq!(error.code, crate::ComputerUseErrorCode::SessionNotStarted);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_start_and_stop_are_idempotent() {
    let harness = RuntimeHarness::with_click_sky();
    let runtime = harness.runtime();

    let first = runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("first start");
    let second = runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("second start");
    let stopped = runtime
        .execute(ComputerUseRequest::Stop(StopArgs {}))
        .await
        .expect("first stop");
    let stopped_again = runtime
        .execute(ComputerUseRequest::Stop(StopArgs {}))
        .await
        .expect("second stop");

    let first_session_id = running_session_id(&first);
    assert_eq!(running_session_id(&second), first_session_id);
    assert_eq!(stopped_session_id(&stopped), first_session_id);
    assert_eq!(stopped_session_id(&stopped_again), first_session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_type_text_routes_through_sky_with_display_environment() {
    let harness = RuntimeHarness::with_click_sky();
    let runtime = harness.runtime();

    let start = runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("start session");
    let session_id = running_session_id(&start);

    let output = runtime
        .execute(ComputerUseRequest::TypeText(TypeTextArgs {
            text: "hello world".to_string(),
        }))
        .await
        .expect("type text through Sky");

    assert_eq!(
        output,
        ComputerUseOutput::Operation {
            session_id: session_id.to_string(),
            operation: InputOperation::TypeText,
        }
    );
    let stdin_log = wait_for_file_contents(&harness.sky_stdin_log)
        .await
        .expect("read Sky stdin log");
    assert!(stdin_log.contains("hello world"));

    let env_log = wait_for_file_contents(&harness.sky_env_log)
        .await
        .expect("read Sky env log");
    assert!(env_log.contains(&format!("display={}", running_environment(&start).display)));
    assert!(env_log.contains("xauthority="));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_screenshot_returns_jpeg_and_cleans_invocation_dir() {
    let harness = RuntimeHarness::with_relative_screenshot_sky();
    let runtime = harness.runtime();

    let start = runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("start session");
    let session_id = running_session_id(&start);

    let output = runtime
        .execute(ComputerUseRequest::GetScreenshot(GetScreenshotArgs {}))
        .await
        .expect("get screenshot");

    match output {
        ComputerUseOutput::Screenshot {
            session_id: output_session_id,
            screenshot,
        } => {
            assert_eq!(output_session_id, session_id);
            assert_eq!(screenshot.width, SCREENSHOT_VIEWPORT_WIDTH);
            assert_eq!(screenshot.height, SCREENSHOT_VIEWPORT_HEIGHT);
            assert_eq!(
                screenshot.bytes,
                fs::read(&harness.fixture_jpeg).expect("read fixture")
            );
        }
        other => panic!("expected screenshot output, got {other:?}"),
    }

    let sky_output_dir = harness.temp_root.join(session_id).join("sky-output");
    let mut entries = fs::read_dir(&sky_output_dir).expect("read sky-output dir");
    assert!(entries.next().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_screenshot_accepts_absolute_path_within_invocation_dir() {
    let harness = RuntimeHarness::with_absolute_screenshot_sky();
    let runtime = harness.runtime();

    runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("start session");

    let output = runtime
        .execute(ComputerUseRequest::GetScreenshot(GetScreenshotArgs {}))
        .await
        .expect("get screenshot");

    match output {
        ComputerUseOutput::Screenshot { screenshot, .. } => {
            assert_eq!(screenshot.width, SCREENSHOT_VIEWPORT_WIDTH);
            assert_eq!(screenshot.height, SCREENSHOT_VIEWPORT_HEIGHT);
            assert_eq!(
                screenshot.bytes,
                fs::read(&harness.fixture_jpeg).expect("read fixture")
            );
        }
        other => panic!("expected screenshot output, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_screenshot_rejects_absolute_path_outside_invocation_dir() {
    let harness = RuntimeHarness::with_outside_absolute_screenshot_sky();
    let runtime = harness.runtime();

    runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("start session");

    let error = runtime
        .execute(ComputerUseRequest::GetScreenshot(GetScreenshotArgs {}))
        .await
        .expect_err("absolute screenshot path outside invocation dir must fail");

    assert_eq!(
        error.code,
        crate::ComputerUseErrorCode::ScreenshotPathInvalid
    );
    assert!(error.message.contains("outside the invocation directory"));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_queued_cancellation_does_not_start_sky() {
    let harness = RuntimeHarness::with_blocking_sky();
    let runtime = harness.runtime();

    runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("start session");

    let running_runtime = runtime.clone();
    let running = tokio::spawn(async move {
        running_runtime
            .execute(ComputerUseRequest::TypeText(TypeTextArgs {
                text: "first request".to_string(),
            }))
            .await
    });
    assert_eq!(
        wait_for_file_contents(&harness.sky_started_log)
            .await
            .expect("first Sky request started"),
        "started\n"
    );

    let (cancellation, canceller) = RequestCancellation::test_pair();
    let queued_runtime = runtime.clone();
    let queued = tokio::spawn(async move {
        queued_runtime
            .execute_with_cancellation(
                ComputerUseRequest::TypeText(TypeTextArgs {
                    text: "cancelled request".to_string(),
                }),
                cancellation,
            )
            .await
    });
    tokio::task::yield_now().await;
    canceller.cancel();

    let error = tokio::time::timeout(Duration::from_secs(1), queued)
        .await
        .expect("queued cancellation completed")
        .expect("queued task did not panic")
        .expect_err("queued request must be cancelled");
    assert_eq!(error.code, crate::ComputerUseErrorCode::SkyCancelled);

    fs::write(&harness.sky_release_file, []).expect("release first Sky request");
    running
        .await
        .expect("running task did not panic")
        .expect("first request completed");
    assert_eq!(
        fs::read_to_string(&harness.sky_started_log).expect("read Sky starts"),
        "started\n"
    );
    assert_eq!(
        fs::read_to_string(&harness.sky_mutation_log).expect("read Sky mutations"),
        "mutated\n"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_running_cancellation_terminates_sky_before_later_mutation() {
    let harness = RuntimeHarness::with_blocking_sky();
    let runtime = harness.runtime();

    let started = runtime
        .execute(ComputerUseRequest::Start(StartArgs {}))
        .await
        .expect("start session");
    let session_id = running_session_id(&started);
    let (cancellation, canceller) = RequestCancellation::test_pair();
    let running_runtime = runtime.clone();
    let running = tokio::spawn(async move {
        running_runtime
            .execute_with_cancellation(
                ComputerUseRequest::TypeText(TypeTextArgs {
                    text: "cancel running request".to_string(),
                }),
                cancellation,
            )
            .await
    });
    assert_eq!(
        wait_for_file_contents(&harness.sky_started_log)
            .await
            .expect("Sky request started"),
        "started\n"
    );
    let sky_pid = wait_for_file_contents(&harness.sky_pid_log)
        .await
        .expect("read Sky pid")
        .trim()
        .parse::<u32>()
        .expect("parse Sky pid");

    canceller.cancel();

    let error = tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .expect("running cancellation completed")
        .expect("running task did not panic")
        .expect_err("running request must be cancelled");
    assert_eq!(error.code, crate::ComputerUseErrorCode::SkyCancelled);
    assert!(
        !Path::new(&format!("/proc/{sky_pid}")).exists(),
        "Sky process must be reaped before cancellation returns"
    );
    let sky_output_dir = harness.temp_root.join(session_id).join("sky-output");
    assert!(
        fs::read_dir(sky_output_dir)
            .expect("read Sky output directory")
            .next()
            .is_none(),
        "Sky invocation directory must be cleaned before cancellation returns"
    );

    fs::write(&harness.sky_release_file, []).expect("release cancelled Sky request");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_file_contents(&harness.sky_mutation_log),
        )
        .await
        .is_err(),
        "terminated Sky process must not mutate after cancellation"
    );
}

struct RuntimeHarness {
    _tempdir: TempDir,
    temp_root: PathBuf,
    xvfb_script: PathBuf,
    openbox_script: PathBuf,
    sky_script: PathBuf,
    sky_env_log: PathBuf,
    sky_stdin_log: PathBuf,
    sky_started_log: PathBuf,
    sky_mutation_log: PathBuf,
    sky_pid_log: PathBuf,
    sky_release_file: PathBuf,
    fixture_jpeg: PathBuf,
}

impl RuntimeHarness {
    fn with_click_sky() -> Self {
        Self::new(SkyScriptMode::Click)
    }

    fn with_relative_screenshot_sky() -> Self {
        Self::new(SkyScriptMode::RelativeScreenshot)
    }

    fn with_absolute_screenshot_sky() -> Self {
        Self::new(SkyScriptMode::AbsoluteScreenshot)
    }

    fn with_outside_absolute_screenshot_sky() -> Self {
        Self::new(SkyScriptMode::OutsideAbsoluteScreenshot)
    }

    fn with_blocking_sky() -> Self {
        Self::new(SkyScriptMode::Blocking)
    }

    fn new(mode: SkyScriptMode) -> Self {
        let tempdir = TempDir::new().expect("create tempdir");
        let temp_root = tempdir.path().join("sessions");
        let xvfb_script = tempdir.path().join("fake-xvfb.sh");
        let openbox_script = tempdir.path().join("fake-openbox.sh");
        let sky_script = tempdir.path().join("fake-sky.sh");
        let sky_env_log = tempdir.path().join("sky-env.log");
        let sky_stdin_log = tempdir.path().join("sky-stdin.log");
        let sky_started_log = tempdir.path().join("sky-started.log");
        let sky_mutation_log = tempdir.path().join("sky-mutation.log");
        let sky_pid_log = tempdir.path().join("sky.pid");
        let sky_release_file = tempdir.path().join("sky-release");
        let fixture_jpeg = tempdir.path().join("fixture.jpg");
        let outside_screenshot = tempdir.path().join("outside-shot.jpg");

        fs::write(
            &fixture_jpeg,
            valid_jpeg(SCREENSHOT_VIEWPORT_WIDTH, SCREENSHOT_VIEWPORT_HEIGHT),
        )
        .expect("write fixture jpeg");
        write_executable(
            &xvfb_script,
            r#"#!/usr/bin/env bash
set -euo pipefail
trap 'exit 0' TERM
while true; do
  sleep 1
done
"#,
        );
        write_executable(
            &openbox_script,
            r#"#!/usr/bin/env bash
set -euo pipefail
trap 'exit 0' TERM
while true; do
  sleep 1
done
"#,
        );
        let sky_script_contents = match mode {
            SkyScriptMode::Click => format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
printf 'display=%s\n' "${{DISPLAY:-}}" >> {sky_env_log:?}
printf 'xauthority=%s\n' "${{XAUTHORITY:-}}" >> {sky_env_log:?}
cat > {sky_stdin_log:?}
"#,
            ),
            SkyScriptMode::RelativeScreenshot => format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
cp {fixture_jpeg:?} "$PWD/shot.jpg"
printf '[{{"filepath":"shot.jpg"}}]'
"#,
            ),
            SkyScriptMode::AbsoluteScreenshot => format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
: "${{TMPDIR:?}}"
cp {fixture_jpeg:?} "$TMPDIR/shot.jpg"
printf '[{{"filepath":"%s"}}]' "$TMPDIR/shot.jpg"
"#,
            ),
            SkyScriptMode::OutsideAbsoluteScreenshot => format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
outside_path={outside_screenshot:?}
cp {fixture_jpeg:?} "$outside_path"
printf '[{{"filepath":"%s"}}]' "$outside_path"
"#,
            ),
            SkyScriptMode::Blocking => format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
trap 'exit 0' TERM
printf '%s\n' "$$" > {sky_pid_log:?}
printf 'started\n' >> {sky_started_log:?}
cat > {sky_stdin_log:?}
while [[ ! -e {sky_release_file:?} ]]; do
  sleep 0.01
done
printf 'mutated\n' >> {sky_mutation_log:?}
"#,
            ),
        };
        write_executable(&sky_script, &sky_script_contents);

        Self {
            _tempdir: tempdir,
            temp_root,
            xvfb_script,
            openbox_script,
            sky_script,
            sky_env_log,
            sky_stdin_log,
            sky_started_log,
            sky_mutation_log,
            sky_pid_log,
            sky_release_file,
            fixture_jpeg,
        }
    }

    fn runtime(&self) -> LocalComputerUseRuntime {
        LocalComputerUseRuntime::new(LocalComputerUseRuntimeConfig {
            session: crate::DesktopSessionConfig {
                command_paths: DesktopCommandPaths {
                    xvfb: self.xvfb_script.clone(),
                    openbox: self.openbox_script.clone(),
                },
                temp_root: self.temp_root.clone(),
                display_ready_timeout: Duration::from_secs(1),
                shutdown_grace_period: Duration::from_secs(1),
            },
            sky_binary_path: self.sky_script.clone(),
        })
    }
}

enum SkyScriptMode {
    Click,
    RelativeScreenshot,
    AbsoluteScreenshot,
    OutsideAbsoluteScreenshot,
    Blocking,
}

fn running_session_id(output: &ComputerUseOutput) -> &str {
    match output {
        ComputerUseOutput::Running { session_id, .. } => session_id,
        other => panic!("expected running output, got {other:?}"),
    }
}

fn running_environment(output: &ComputerUseOutput) -> &crate::protocol::DesktopEnvironment {
    match output {
        ComputerUseOutput::Running { environment, .. } => environment,
        other => panic!("expected running output, got {other:?}"),
    }
}

fn stopped_session_id(output: &ComputerUseOutput) -> &str {
    match output {
        ComputerUseOutput::Stopped { session_id } => session_id,
        other => panic!("expected stopped output, got {other:?}"),
    }
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("chmod script");
}

async fn wait_for_file_contents(path: &Path) -> std::io::Result<String> {
    for _ in 0..20 {
        match fs::read_to_string(path) {
            Ok(contents) => return Ok(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error),
        }
    }
    fs::read_to_string(path)
}

fn valid_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(width, height, Rgb([1, 2, 3])));
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 90);
    encoder.encode_image(&image).expect("encode jpeg");
    bytes
}
