use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rand::RngCore as _;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio::time::timeout;

use crate::ComputerUseRuntime;
use crate::protocol::ComputerUseError;
use crate::protocol::ComputerUseErrorCode;
use crate::protocol::ComputerUseOutput;
use crate::protocol::ComputerUseRequest;
use crate::protocol::DesktopEnvironment;
use crate::protocol::InputOperation;
use crate::session::DesktopSession;
use crate::session::DesktopSessionConfig;
use crate::sky::SkyInvocation;
use crate::sky::parse_screenshot_stdout;
use crate::sky::sky_invocation_for_request;
use crate::sky::validated_screenshot_payload_from_jpeg;

const SKY_STDOUT_LIMIT_BYTES: usize = 1_024 * 1_024;
const SKY_STDERR_CAPTURE_LIMIT_BYTES: usize = 64 * 1_024;
const SKY_OUTER_TIMEOUT: Duration = Duration::from_secs(32);
const SKY_TERM_GRACE_PERIOD: Duration = Duration::from_secs(2);
const SCREENSHOT_MAX_BYTES: u64 = 8 * 1_024 * 1_024;
const INVOCATION_DIR_MODE: u32 = 0o700;

#[derive(Debug, Clone)]
pub struct LocalComputerUseRuntimeConfig {
    pub session: DesktopSessionConfig,
    pub sky_binary_path: PathBuf,
}

impl Default for LocalComputerUseRuntimeConfig {
    fn default() -> Self {
        Self {
            session: DesktopSessionConfig::default(),
            sky_binary_path: PathBuf::from("sky"),
        }
    }
}

#[derive(Clone)]
pub struct LocalComputerUseRuntime {
    config: Arc<LocalComputerUseRuntimeConfig>,
    state: Arc<Mutex<RuntimeState>>,
}

impl LocalComputerUseRuntime {
    pub fn new(config: LocalComputerUseRuntimeConfig) -> Self {
        Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(RuntimeState::default())),
        }
    }
}

impl ComputerUseRuntime for LocalComputerUseRuntime {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "Computer Use session state transitions must remain serialized"
    )]
    async fn execute(
        &self,
        request: ComputerUseRequest,
    ) -> Result<ComputerUseOutput, ComputerUseError> {
        let mut state = self.state.lock().await;
        state.execute(&self.config, request).await
    }
}

#[derive(Default)]
struct RuntimeState {
    last_session_id: Option<String>,
    session: Option<DesktopSession>,
}

impl RuntimeState {
    async fn execute(
        &mut self,
        config: &LocalComputerUseRuntimeConfig,
        request: ComputerUseRequest,
    ) -> Result<ComputerUseOutput, ComputerUseError> {
        match request {
            ComputerUseRequest::Start(_) => self.start_or_reuse_session(config).await,
            ComputerUseRequest::GetEnvironment(_) => self.current_environment().await,
            ComputerUseRequest::Stop(_) => self.stop_session().await,
            tool_request => self.run_sky_tool(config, tool_request).await,
        }
    }

    async fn start_or_reuse_session(
        &mut self,
        config: &LocalComputerUseRuntimeConfig,
    ) -> Result<ComputerUseOutput, ComputerUseError> {
        if let Some(session) = self.session.as_mut()
            && session.ensure_healthy().await.is_ok()
        {
            return Ok(ComputerUseOutput::Running {
                session_id: session.session_id().to_string(),
                environment: session.environment().clone(),
            });
        }

        self.stop_current_session_best_effort().await;
        let session = DesktopSession::start(&config.session).await?;
        let session_id = session.session_id().to_string();
        let environment = session.environment().clone();
        self.last_session_id = Some(session_id.clone());
        self.session = Some(session);
        Ok(ComputerUseOutput::Running {
            session_id,
            environment,
        })
    }

    async fn current_environment(&mut self) -> Result<ComputerUseOutput, ComputerUseError> {
        let Some(session) = self.session.as_mut() else {
            return Err(session_not_started_error());
        };
        if let Err(error) = session.ensure_healthy().await {
            self.stop_current_session_best_effort().await;
            return Err(error);
        }

        Ok(ComputerUseOutput::Running {
            session_id: session.session_id().to_string(),
            environment: session.environment().clone(),
        })
    }

    async fn stop_session(&mut self) -> Result<ComputerUseOutput, ComputerUseError> {
        let Some(session) = self.session.take() else {
            return self
                .last_session_id
                .clone()
                .map(|session_id| ComputerUseOutput::Stopped { session_id })
                .ok_or_else(session_not_started_error);
        };

        let fallback_session_id = session.session_id().to_string();
        let stopped_session_id = match session.stop().await {
            Ok(session_id) => session_id,
            Err(error) => {
                self.last_session_id = Some(fallback_session_id);
                return Err(error);
            }
        };
        self.last_session_id = Some(stopped_session_id.clone());
        Ok(ComputerUseOutput::Stopped {
            session_id: stopped_session_id,
        })
    }

    async fn run_sky_tool(
        &mut self,
        config: &LocalComputerUseRuntimeConfig,
        request: ComputerUseRequest,
    ) -> Result<ComputerUseOutput, ComputerUseError> {
        let Some(session) = self.session.as_mut() else {
            return Err(session_not_started_error());
        };
        if let Err(error) = session.ensure_healthy().await {
            self.stop_current_session_best_effort().await;
            return Err(error);
        }

        let sky_invocation = sky_invocation_for_request(config.sky_binary_path.clone(), &request)
            .ok_or_else(|| {
            ComputerUseError::new(
                ComputerUseErrorCode::InternalError,
                format!(
                    "{} is not a Sky-backed Computer Use request",
                    request.tool_name()
                ),
                /*retryable*/ false,
            )
        })?;
        let session_id = session.session_id().to_string();
        let environment = session.environment().clone();
        let sky_output_root = session.sky_output_dir().to_path_buf();

        run_sky_invocation(
            &session_id,
            &environment,
            &sky_output_root,
            request,
            sky_invocation,
        )
        .await
    }

    async fn stop_current_session_best_effort(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.last_session_id = Some(session.session_id().to_string());
        let _ = session.stop().await;
    }
}

async fn run_sky_invocation(
    session_id: &str,
    environment: &DesktopEnvironment,
    sky_output_root: &Path,
    request: ComputerUseRequest,
    sky_invocation: SkyInvocation,
) -> Result<ComputerUseOutput, ComputerUseError> {
    let invocation_dir = create_private_invocation_dir(sky_output_root).map_err(internal_error)?;
    let result = run_sky_invocation_inner(
        session_id,
        environment,
        &invocation_dir,
        request,
        sky_invocation,
    )
    .await;

    let cleanup_result = cleanup_invocation_dir(&invocation_dir).map_err(internal_error);
    match (result, cleanup_result) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (_, Err(cleanup_error)) => Err(cleanup_error),
    }
}

async fn run_sky_invocation_inner(
    session_id: &str,
    environment: &DesktopEnvironment,
    invocation_dir: &Path,
    request: ComputerUseRequest,
    sky_invocation: SkyInvocation,
) -> Result<ComputerUseOutput, ComputerUseError> {
    let stdin_bytes = serde_json::to_vec(&sky_invocation.stdin_json).map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::InternalError,
            format!("failed to serialize Sky stdin JSON: {error}"),
            /*retryable*/ false,
        )
    })?;
    let operation = input_operation(&request);
    let mut child = spawn_sky_child(environment, invocation_dir, &sky_invocation)?;
    let process_group_id = child.id().ok_or_else(|| {
        ComputerUseError::new(
            ComputerUseErrorCode::SkyFailed,
            "failed to determine the Sky process id",
            /*retryable*/ true,
        )
    })?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        ComputerUseError::new(
            ComputerUseErrorCode::InternalError,
            "Sky stdin pipe was unavailable",
            /*retryable*/ true,
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ComputerUseError::new(
            ComputerUseErrorCode::InternalError,
            "Sky stdout pipe was unavailable",
            /*retryable*/ true,
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ComputerUseError::new(
            ComputerUseErrorCode::InternalError,
            "Sky stderr pipe was unavailable",
            /*retryable*/ true,
        )
    })?;

    let stdin_task = tokio::spawn(async move {
        stdin.write_all(&stdin_bytes).await?;
        stdin.shutdown().await
    });
    let stdout_task = tokio::spawn(read_bounded_stream(stdout, SKY_STDOUT_LIMIT_BYTES));
    let stderr_task = tokio::spawn(read_bounded_stream(stderr, SKY_STDERR_CAPTURE_LIMIT_BYTES));

    let status = match timeout(SKY_OUTER_TIMEOUT, child.wait()).await {
        Ok(wait_result) => wait_result.map_err(|error| {
            ComputerUseError::new(
                ComputerUseErrorCode::SkyFailed,
                format!("failed to wait for the Sky process: {error}"),
                /*retryable*/ true,
            )
        })?,
        Err(_) => {
            let _ = terminate_process_group(&mut child, process_group_id).await;
            let _ = stdin_task.await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(ComputerUseError::new(
                ComputerUseErrorCode::SkyTimeout,
                "Sky exceeded the 32-second outer deadline",
                /*retryable*/ true,
            ));
        }
    };

    let stdin_result = stdin_task.await.map_err(join_internal_error)?;
    let stdout_result = stdout_task
        .await
        .map_err(join_internal_error)?
        .map_err(stream_read_error)?;
    let stderr_result = stderr_task
        .await
        .map_err(join_internal_error)?
        .map_err(stream_read_error)?;
    if let Err(error) = stdin_result {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::SkyFailed,
            format!("failed to write Sky stdin: {error}"),
            /*retryable*/ true,
        ));
    }
    if stdout_result.truncated {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::SkyOutputTooLarge,
            format!("Sky stdout exceeded {SKY_STDOUT_LIMIT_BYTES} bytes"),
            /*retryable*/ false,
        ));
    }
    let stdout_text = String::from_utf8(stdout_result.bytes).map_err(|error| {
        ComputerUseError::sky_protocol_error(format!("Sky emitted non-UTF-8 stdout: {error}"))
    })?;
    if !status.success() {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::SkyFailed,
            format!(
                "Sky exited with status {status}; stderr: {}",
                format_stderr(&stderr_result),
            ),
            /*retryable*/ true,
        ));
    }

    if let Some(post_action_sleep) = sky_invocation.post_action_sleep {
        sleep(post_action_sleep).await;
    }

    match operation {
        Some(operation) => Ok(ComputerUseOutput::Operation {
            session_id: session_id.to_string(),
            operation,
        }),
        None => {
            let reported_path = parse_screenshot_stdout(&stdout_text)?;
            let screenshot = load_screenshot_payload(invocation_dir, &reported_path)?;
            Ok(ComputerUseOutput::Screenshot {
                session_id: session_id.to_string(),
                screenshot,
            })
        }
    }
}

fn spawn_sky_child(
    environment: &DesktopEnvironment,
    invocation_dir: &Path,
    sky_invocation: &SkyInvocation,
) -> Result<Child, ComputerUseError> {
    let mut command = Command::new(&sky_invocation.binary_path);
    command
        .kill_on_drop(true)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(invocation_dir)
        .env_clear()
        .env("DISPLAY", &environment.display)
        .env("XAUTHORITY", &environment.xauthority)
        .env("HOME", invocation_dir)
        .env("TMPDIR", invocation_dir)
        .env("LANG", "C.UTF-8")
        .args(&sky_invocation.argv);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let parent_pid = unsafe { libc::getpid() };
    unsafe {
        command
            .pre_exec(move || codex_utils_pty::process_group::set_parent_death_signal(parent_pid));
    }

    command.spawn().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => ComputerUseError::new(
            ComputerUseErrorCode::RuntimeUnavailable,
            format!(
                "Sky runtime is unavailable at {}: {error}",
                sky_invocation.binary_path.display()
            ),
            /*retryable*/ false,
        ),
        _ => ComputerUseError::new(
            ComputerUseErrorCode::SkyFailed,
            format!("failed to spawn the Sky runtime: {error}"),
            /*retryable*/ true,
        ),
    })
}

async fn terminate_process_group(child: &mut Child, process_group_id: u32) -> io::Result<()> {
    let _ = codex_utils_pty::process_group::terminate_process_group(process_group_id);
    match timeout(SKY_TERM_GRACE_PERIOD, child.wait()).await {
        Ok(wait_result) => wait_result.map(|_| ()),
        Err(_) => {
            let _ = codex_utils_pty::process_group::kill_process_group(process_group_id);
            match timeout(SKY_TERM_GRACE_PERIOD, child.wait()).await {
                Ok(wait_result) => wait_result.map(|_| ()),
                Err(_) => Ok(()),
            }
        }
    }
}

async fn read_bounded_stream<R>(
    mut reader: R,
    capture_limit_bytes: usize,
) -> io::Result<CollectedStream>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8_192];
    let mut truncated = false;
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        let remaining_capacity = capture_limit_bytes.saturating_sub(bytes.len());
        let to_copy = remaining_capacity.min(bytes_read);
        if to_copy > 0 {
            bytes.extend_from_slice(&buffer[..to_copy]);
        }
        if bytes_read > remaining_capacity {
            truncated = true;
        }
    }
    Ok(CollectedStream { bytes, truncated })
}

fn create_private_invocation_dir(root: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    for _ in 0..16 {
        let candidate = root.join(format!("invocation-{:016x}", random_u64()));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                fs::set_permissions(&candidate, fs::Permissions::from_mode(INVOCATION_DIR_MODE))?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to allocate a unique Sky invocation directory",
    ))
}

fn cleanup_invocation_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn load_screenshot_payload(
    invocation_dir: &Path,
    reported_path: &str,
) -> Result<crate::ScreenshotPayload, ComputerUseError> {
    let reported_path = PathBuf::from(reported_path);
    if !reported_path.is_absolute()
        && reported_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotPathInvalid,
            "Sky reported a screenshot path outside the invocation directory",
            /*retryable*/ false,
        ));
    }

    let absolute_path = if reported_path.is_absolute() {
        reported_path
    } else {
        invocation_dir.join(&reported_path)
    };
    let invocation_dir_canonical = invocation_dir.canonicalize().map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::InternalError,
            format!("failed to resolve the invocation directory: {error}"),
            /*retryable*/ true,
        )
    })?;
    let screenshot_canonical = absolute_path.canonicalize().map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            format!("failed to resolve the screenshot file: {error}"),
            /*retryable*/ false,
        )
    })?;
    if !screenshot_canonical.starts_with(&invocation_dir_canonical) {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotPathInvalid,
            "Sky reported a screenshot path outside the invocation directory",
            /*retryable*/ false,
        ));
    }

    let symlink_metadata = fs::symlink_metadata(&absolute_path).map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            format!("failed to inspect the screenshot file: {error}"),
            /*retryable*/ false,
        )
    })?;
    if symlink_metadata.file_type().is_symlink() || !symlink_metadata.file_type().is_file() {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            "Sky reported a non-regular screenshot file",
            /*retryable*/ false,
        ));
    }

    let file = fs::File::open(&absolute_path).map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            format!("failed to open the screenshot file: {error}"),
            /*retryable*/ false,
        )
    })?;
    let open_metadata = file.metadata().map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            format!("failed to inspect the opened screenshot file: {error}"),
            /*retryable*/ false,
        )
    })?;
    if symlink_metadata.dev() != open_metadata.dev()
        || symlink_metadata.ino() != open_metadata.ino()
    {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            "the screenshot file changed before it was read",
            /*retryable*/ false,
        ));
    }
    if open_metadata.len() > SCREENSHOT_MAX_BYTES {
        return Err(ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotTooLarge,
            format!("screenshot exceeded {SCREENSHOT_MAX_BYTES} bytes"),
            /*retryable*/ false,
        ));
    }

    let bytes = fs::read(&absolute_path).map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotFileInvalid,
            format!("failed to read the screenshot file: {error}"),
            /*retryable*/ false,
        )
    })?;
    let payload = validated_screenshot_payload_from_jpeg(&bytes)?;
    fs::remove_file(&absolute_path).map_err(|error| {
        ComputerUseError::new(
            ComputerUseErrorCode::ScreenshotCleanupFailed,
            format!("failed to delete the screenshot file: {error}"),
            /*retryable*/ true,
        )
    })?;
    Ok(payload)
}

fn input_operation(request: &ComputerUseRequest) -> Option<InputOperation> {
    match request {
        ComputerUseRequest::Click(_) => Some(InputOperation::Click),
        ComputerUseRequest::Drag(_) => Some(InputOperation::Drag),
        ComputerUseRequest::Move(_) => Some(InputOperation::Move),
        ComputerUseRequest::PressKey(_) => Some(InputOperation::PressKey),
        ComputerUseRequest::Scroll(_) => Some(InputOperation::Scroll),
        ComputerUseRequest::TypeText(_) => Some(InputOperation::TypeText),
        ComputerUseRequest::GetScreenshot(_)
        | ComputerUseRequest::GetEnvironment(_)
        | ComputerUseRequest::Start(_)
        | ComputerUseRequest::Stop(_) => None,
    }
}

fn format_stderr(stderr: &CollectedStream) -> String {
    let mut rendered = String::from_utf8_lossy(&stderr.bytes).trim().to_string();
    if rendered.is_empty() {
        rendered = "<empty>".to_string();
    }
    if stderr.truncated {
        rendered.push_str(" [truncated]");
    }
    rendered
}

fn random_u64() -> u64 {
    let mut bytes = [0_u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    u64::from_be_bytes(bytes)
}

fn join_internal_error(error: tokio::task::JoinError) -> ComputerUseError {
    ComputerUseError::new(
        ComputerUseErrorCode::InternalError,
        format!("internal task join failed: {error}"),
        /*retryable*/ true,
    )
}

fn internal_error(error: io::Error) -> ComputerUseError {
    ComputerUseError::new(
        ComputerUseErrorCode::InternalError,
        format!("internal Computer Use cleanup failed: {error}"),
        /*retryable*/ true,
    )
}

fn stream_read_error(error: io::Error) -> ComputerUseError {
    ComputerUseError::new(
        ComputerUseErrorCode::SkyFailed,
        format!("failed to collect Sky output: {error}"),
        /*retryable*/ true,
    )
}

fn session_not_started_error() -> ComputerUseError {
    ComputerUseError::new(
        ComputerUseErrorCode::SessionNotStarted,
        "Computer Use has not started a desktop session yet",
        /*retryable*/ false,
    )
}

struct CollectedStream {
    bytes: Vec<u8>,
    truncated: bool,
}
