#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::process::Command;

use crate::DesktopSession;
use crate::DesktopSessionConfig;

#[tokio::test(flavor = "current_thread")]
async fn start_creates_private_session_and_xauthority() {
    let harness = FakeDesktopHarness::startable("77");
    let config = harness.config();

    let session = DesktopSession::start(&config)
        .await
        .expect("start fake desktop session");

    assert!(session.session_id().starts_with("computer-use-"));
    assert_eq!(session.environment().display, ":77");
    assert_eq!(
        session.environment().xauthority,
        session.xauthority_path().display().to_string()
    );
    assert_eq!(
        session_mode(session.session_dir()).expect("session dir mode"),
        0o700
    );
    assert_eq!(
        session_mode(session.sky_output_dir()).expect("sky output dir mode"),
        0o700
    );
    assert_eq!(
        session_mode(session.xauthority_path()).expect("xauthority mode"),
        0o600
    );

    let xauthority_record =
        parse_xauthority_record(&fs::read(session.xauthority_path()).expect("read xauthority"));
    assert_eq!(xauthority_record.family, 0xffff);
    assert!(xauthority_record.address.is_empty());
    assert!(xauthority_record.display.is_empty());
    assert_eq!(xauthority_record.protocol_name, "MIT-MAGIC-COOKIE-1");
    assert_eq!(xauthority_record.cookie.len(), 16);

    let xvfb_log = fs::read_to_string(&harness.xvfb_log).expect("read xvfb log");
    assert!(xvfb_log.contains("-displayfd"));
    assert!(xvfb_log.contains("-screen"));
    assert!(xvfb_log.contains("1440x900x24"));
    assert!(xvfb_log.contains("-dpi"));
    assert!(xvfb_log.contains("96"));
    assert!(xvfb_log.contains("-nolisten"));
    assert!(xvfb_log.contains("-auth"));
    assert!(xvfb_log.contains(&session.xauthority_path().display().to_string()));

    let openbox_log = wait_for_file_contents(&harness.openbox_log)
        .await
        .expect("read openbox log");
    assert!(openbox_log.contains("display=:77"));
    assert!(openbox_log.contains(&format!(
        "xauthority={}",
        session.xauthority_path().display()
    )));

    session.stop().await.expect("stop fake desktop session");
}

#[tokio::test(flavor = "current_thread")]
async fn stop_terminates_owned_children_and_preserves_unrelated_process() {
    let harness = FakeDesktopHarness::startable("88");
    let config = harness.config();

    let session = DesktopSession::start(&config)
        .await
        .expect("start fake desktop session");
    let session_id = session.session_id().to_string();
    let session_dir = session.session_dir().to_path_buf();

    let mut sentinel = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn unrelated sentinel");

    let _ = wait_for_file_contents(&harness.openbox_log)
        .await
        .expect("read openbox log before stop");

    let stopped_session_id = session.stop().await.expect("stop fake session");
    assert_eq!(stopped_session_id, session_id);
    assert!(!session_dir.exists());
    assert!(sentinel.try_wait().expect("inspect sentinel").is_none());

    let xvfb_log = wait_for_file_contents(&harness.xvfb_log)
        .await
        .expect("read xvfb log");
    let openbox_log = wait_for_file_contents(&harness.openbox_log)
        .await
        .expect("read openbox log");
    assert!(xvfb_log.contains("term"));
    assert!(openbox_log.contains("term"));

    sentinel.kill().await.expect("kill unrelated sentinel");
}

#[tokio::test(flavor = "current_thread")]
async fn start_fails_when_xvfb_never_reports_display() {
    let harness = FakeDesktopHarness::xvfb_exits_early();
    let config = harness.config();

    let error = DesktopSession::start(&config)
        .await
        .expect_err("xvfb must fail without a display");

    assert_eq!(error.code, crate::ComputerUseErrorCode::SessionStartFailed);
    assert!(
        error
            .message
            .contains("Xvfb exited before reporting a display number")
            || error.message.contains("startup timeout"),
        "unexpected error message: {}",
        error.message
    );
}

struct FakeDesktopHarness {
    _tempdir: TempDir,
    temp_root: PathBuf,
    xvfb_script: PathBuf,
    openbox_script: PathBuf,
    xvfb_log: PathBuf,
    openbox_log: PathBuf,
}

impl FakeDesktopHarness {
    fn startable(display_number: &str) -> Self {
        let tempdir = TempDir::new().expect("create tempdir");
        let temp_root = tempdir.path().join("sessions");
        let xvfb_log = tempdir.path().join("xvfb.log");
        let openbox_log = tempdir.path().join("openbox.log");

        let xvfb_script = tempdir.path().join("fake-xvfb.sh");
        let openbox_script = tempdir.path().join("fake-openbox.sh");
        write_executable(
            &xvfb_script,
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
log={xvfb_log:?}
displayfd=""
while (($#)); do
  case "$1" in
    -displayfd)
      displayfd="$2"
      ;;
  esac
  printf '%s\n' "$1" >> "$log"
  shift
done
printf '%s\n' "{display_number}" >&"$displayfd"
trap 'echo term >> "$log"; exit 0' TERM
while true; do
  sleep 1
done
"#,
            ),
        );
        write_executable(
            &openbox_script,
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
log={openbox_log:?}
printf 'display=%s\n' "${{DISPLAY:-}}" >> "$log"
printf 'xauthority=%s\n' "${{XAUTHORITY:-}}" >> "$log"
trap 'echo term >> "$log"; exit 0' TERM
while true; do
  sleep 1
done
"#,
            ),
        );

        Self {
            _tempdir: tempdir,
            temp_root,
            xvfb_script,
            openbox_script,
            xvfb_log,
            openbox_log,
        }
    }

    fn xvfb_exits_early() -> Self {
        let tempdir = TempDir::new().expect("create tempdir");
        let temp_root = tempdir.path().join("sessions");
        let xvfb_log = tempdir.path().join("xvfb.log");
        let openbox_log = tempdir.path().join("openbox.log");
        let xvfb_script = tempdir.path().join("fake-xvfb.sh");
        let openbox_script = tempdir.path().join("fake-openbox.sh");
        write_executable(
            &xvfb_script,
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
log={xvfb_log:?}
printf 'exit-early\n' >> "$log"
exit 17
"#,
            ),
        );
        write_executable(
            &openbox_script,
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
log={openbox_log:?}
printf 'unexpected-openbox-start\n' >> "$log"
exit 19
"#,
            ),
        );

        Self {
            _tempdir: tempdir,
            temp_root,
            xvfb_script,
            openbox_script,
            xvfb_log,
            openbox_log,
        }
    }

    fn config(&self) -> DesktopSessionConfig {
        DesktopSessionConfig {
            command_paths: crate::DesktopCommandPaths {
                xvfb: self.xvfb_script.clone(),
                openbox: self.openbox_script.clone(),
            },
            temp_root: self.temp_root.clone(),
            display_ready_timeout: Duration::from_secs(1),
            shutdown_grace_period: Duration::from_secs(1),
        }
    }
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("chmod script");
}

fn session_mode(path: &Path) -> std::io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
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

#[derive(Debug, PartialEq, Eq)]
struct XauthorityRecord {
    family: u16,
    address: Vec<u8>,
    display: Vec<u8>,
    protocol_name: String,
    cookie: Vec<u8>,
}

fn parse_xauthority_record(bytes: &[u8]) -> XauthorityRecord {
    let mut cursor = 0_usize;
    let family = read_u16(bytes, &mut cursor);
    let address_len = usize::from(read_u16(bytes, &mut cursor));
    let address = read_bytes(bytes, &mut cursor, address_len);
    let display_len = usize::from(read_u16(bytes, &mut cursor));
    let display = read_bytes(bytes, &mut cursor, display_len);
    let protocol_name_len = usize::from(read_u16(bytes, &mut cursor));
    let protocol_name = String::from_utf8(read_bytes(bytes, &mut cursor, protocol_name_len))
        .expect("protocol name utf8");
    let cookie_len = usize::from(read_u16(bytes, &mut cursor));
    let cookie = read_bytes(bytes, &mut cursor, cookie_len);
    assert_eq!(cursor, bytes.len());

    XauthorityRecord {
        family,
        address,
        display,
        protocol_name,
        cookie,
    }
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> u16 {
    let value = u16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]);
    *cursor += 2;
    value
}

fn read_bytes(bytes: &[u8], cursor: &mut usize, len: usize) -> Vec<u8> {
    let value = bytes[*cursor..*cursor + len].to_vec();
    *cursor += len;
    value
}
