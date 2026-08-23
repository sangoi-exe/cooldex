#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod imp {
    use std::fs;
    use std::fs::File;
    use std::fs::OpenOptions;
    use std::io;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use rand::RngCore as _;
    use rand::TryRngCore;
    use rand::rngs::OsRng;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;
    use tokio::process::Child;
    use tokio::process::Command;
    use tokio::time::timeout;

    use crate::protocol::ComputerUseError;
    use crate::protocol::ComputerUseErrorCode;
    use crate::protocol::DesktopEnvironment;
    use crate::protocol::SCREENSHOT_VIEWPORT_DEPTH;
    use crate::protocol::SCREENSHOT_VIEWPORT_DPI;
    use crate::protocol::SCREENSHOT_VIEWPORT_HEIGHT;
    use crate::protocol::SCREENSHOT_VIEWPORT_WIDTH;

    const DEFAULT_DISPLAY_READY_TIMEOUT: Duration = Duration::from_secs(5);
    const DEFAULT_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);
    const SESSION_DIR_MODE: u32 = 0o700;
    const XAUTHORITY_MODE: u32 = 0o600;
    const XAUTHORITY_PROTOCOL_NAME: &str = "MIT-MAGIC-COOKIE-1";
    const XAUTHORITY_COOKIE_BYTES: usize = 16;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DesktopCommandPaths {
        pub xvfb: PathBuf,
        pub openbox: PathBuf,
    }

    impl Default for DesktopCommandPaths {
        fn default() -> Self {
            Self {
                xvfb: PathBuf::from("Xvfb"),
                openbox: PathBuf::from("openbox"),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DesktopSessionConfig {
        pub command_paths: DesktopCommandPaths,
        pub temp_root: PathBuf,
        pub display_ready_timeout: Duration,
        pub shutdown_grace_period: Duration,
    }

    impl Default for DesktopSessionConfig {
        fn default() -> Self {
            Self {
                command_paths: DesktopCommandPaths::default(),
                temp_root: std::env::temp_dir().join("codex-computer-use"),
                display_ready_timeout: DEFAULT_DISPLAY_READY_TIMEOUT,
                shutdown_grace_period: DEFAULT_SHUTDOWN_GRACE_PERIOD,
            }
        }
    }

    #[derive(Debug)]
    pub struct DesktopSession {
        session_id: String,
        session_dir: PathBuf,
        xauthority_path: PathBuf,
        sky_output_dir: PathBuf,
        environment: DesktopEnvironment,
        openbox: OwnedChild,
        xvfb: OwnedChild,
        shutdown_grace_period: Duration,
    }

    impl DesktopSession {
        pub async fn start(config: &DesktopSessionConfig) -> Result<Self, ComputerUseError> {
            fs::create_dir_all(&config.temp_root).map_err(|error| {
                ComputerUseError::new(
                    ComputerUseErrorCode::SessionStartFailed,
                    format!("failed to create Computer Use temp root: {error}"),
                    /*retryable*/ true,
                )
            })?;

            let session_id = generate_session_id();
            let session_dir = config.temp_root.join(&session_id);
            create_private_dir(&session_dir).map_err(session_start_failed)?;
            let xauthority_path = session_dir.join(".Xauthority");
            if let Err(error) = write_xauthority_file(&xauthority_path) {
                let _ = cleanup_private_session_dir(&session_dir);
                return Err(session_start_failed(error));
            }
            let sky_output_dir = session_dir.join("sky-output");
            if let Err(error) = create_private_dir(&sky_output_dir) {
                let _ = cleanup_private_session_dir(&session_dir);
                return Err(session_start_failed(error));
            }

            let (display_reader, display_writer) = match create_inherited_pipe() {
                Ok(pipe) => pipe,
                Err(error) => {
                    let _ = cleanup_private_session_dir(&session_dir);
                    return Err(session_start_failed(error));
                }
            };

            let mut xvfb = match spawn_xvfb(
                &config.command_paths.xvfb,
                &session_dir,
                &xauthority_path,
                display_writer.as_raw_fd(),
            ) {
                Ok(xvfb) => xvfb,
                Err(error) => {
                    let _ = cleanup_private_session_dir(&session_dir);
                    return Err(error);
                }
            };
            drop(display_writer);

            let display = match read_display(display_reader, config.display_ready_timeout).await {
                Ok(display) => display,
                Err(error) => {
                    let _ = xvfb.terminate(config.shutdown_grace_period).await;
                    let _ = cleanup_private_session_dir(&session_dir);
                    return Err(error);
                }
            };

            let environment =
                DesktopEnvironment::new(display.clone(), xauthority_path.display().to_string());
            if let Err(error) = xvfb.ensure_running("Xvfb").await {
                let _ = cleanup_private_session_dir(&session_dir);
                return Err(error);
            }

            let mut openbox =
                match spawn_openbox(&config.command_paths.openbox, &session_dir, &environment) {
                    Ok(openbox) => openbox,
                    Err(error) => {
                        let _ = xvfb.terminate(config.shutdown_grace_period).await;
                        let _ = cleanup_private_session_dir(&session_dir);
                        return Err(error);
                    }
                };
            if let Err(error) = openbox.ensure_running("Openbox").await {
                let _ = openbox.terminate(config.shutdown_grace_period).await;
                let _ = xvfb.terminate(config.shutdown_grace_period).await;
                let _ = cleanup_private_session_dir(&session_dir);
                return Err(error);
            }

            Ok(Self {
                session_id,
                session_dir,
                xauthority_path,
                sky_output_dir,
                environment,
                openbox,
                xvfb,
                shutdown_grace_period: config.shutdown_grace_period,
            })
        }

        pub fn session_id(&self) -> &str {
            &self.session_id
        }

        pub fn environment(&self) -> &DesktopEnvironment {
            &self.environment
        }

        pub fn session_dir(&self) -> &Path {
            &self.session_dir
        }

        pub fn xauthority_path(&self) -> &Path {
            &self.xauthority_path
        }

        pub fn sky_output_dir(&self) -> &Path {
            &self.sky_output_dir
        }

        pub async fn ensure_healthy(&mut self) -> Result<(), ComputerUseError> {
            self.xvfb.ensure_running("Xvfb").await?;
            self.openbox.ensure_running("Openbox").await?;
            Ok(())
        }

        pub async fn stop(mut self) -> Result<String, ComputerUseError> {
            self.openbox
                .terminate(self.shutdown_grace_period)
                .await
                .map_err(internal_error)?;
            self.xvfb
                .terminate(self.shutdown_grace_period)
                .await
                .map_err(internal_error)?;
            cleanup_private_session_dir(&self.session_dir).map_err(internal_error)?;
            Ok(self.session_id)
        }
    }

    #[derive(Debug)]
    struct OwnedChild {
        child: Child,
        process_group_id: u32,
    }

    impl OwnedChild {
        fn new(child: Child) -> Result<Self, ComputerUseError> {
            let process_group_id = child.id().ok_or_else(|| {
                ComputerUseError::new(
                    ComputerUseErrorCode::SessionStartFailed,
                    "failed to determine child process id",
                    /*retryable*/ true,
                )
            })?;
            Ok(Self {
                child,
                process_group_id,
            })
        }

        async fn ensure_running(&mut self, label: &str) -> Result<(), ComputerUseError> {
            match self.child.try_wait().map_err(session_start_failed)? {
                Some(status) => Err(ComputerUseError::new(
                    ComputerUseErrorCode::SessionUnhealthy,
                    format!("{label} exited unexpectedly with status {status}"),
                    /*retryable*/ true,
                )),
                None => Ok(()),
            }
        }

        async fn terminate(&mut self, grace_period: Duration) -> io::Result<()> {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }

            let _ = codex_utils_pty::process_group::terminate_process_group(self.process_group_id);

            if timeout(grace_period, self.child.wait()).await.is_err() {
                let _ = codex_utils_pty::process_group::kill_process_group(self.process_group_id);
                let _ = timeout(grace_period, self.child.wait()).await;
            }

            Ok(())
        }
    }

    fn create_private_dir(path: &Path) -> io::Result<()> {
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(SESSION_DIR_MODE))?;
        Ok(())
    }

    fn cleanup_private_session_dir(path: &Path) -> io::Result<()> {
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    fn generate_session_id() -> String {
        let mut bytes = [0_u8; 8];
        rand::rng().fill_bytes(&mut bytes);
        format!("computer-use-{:016x}", u64::from_be_bytes(bytes))
    }

    fn write_xauthority_file(path: &Path) -> io::Result<()> {
        let mut cookie = [0_u8; XAUTHORITY_COOKIE_BYTES];
        let mut rng = OsRng;
        rng.try_fill_bytes(&mut cookie).map_err(|error| {
            io::Error::other(format!("failed to generate Xauthority cookie: {error}"))
        })?;

        let mut file = OpenOptions::new()
            .create_new(true)
            .mode(XAUTHORITY_MODE)
            .write(true)
            .open(path)?;
        write_xauthority_record(&mut file, &cookie)?;
        file.flush()?;
        Ok(())
    }

    fn write_xauthority_record(
        writer: &mut File,
        cookie: &[u8; XAUTHORITY_COOKIE_BYTES],
    ) -> io::Result<()> {
        write_be_u16(writer, 0xffff)?;
        write_be_u16(writer, 0)?;
        write_be_u16(writer, 0)?;
        write_be_u16(
            writer,
            u16::try_from(XAUTHORITY_PROTOCOL_NAME.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "protocol name too long")
            })?,
        )?;
        writer.write_all(XAUTHORITY_PROTOCOL_NAME.as_bytes())?;
        write_be_u16(
            writer,
            u16::try_from(cookie.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "cookie length is invalid")
            })?,
        )?;
        writer.write_all(cookie)?;
        Ok(())
    }

    fn write_be_u16(writer: &mut File, value: u16) -> io::Result<()> {
        writer.write_all(&value.to_be_bytes())
    }

    fn create_inherited_pipe() -> io::Result<(File, OwnedFd)> {
        let mut fds = [0_i32; 2];
        let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }

        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        Ok((File::from(read_fd), write_fd))
    }

    async fn read_display(
        display_reader: File,
        ready_timeout: Duration,
    ) -> Result<String, ComputerUseError> {
        let reader = tokio::fs::File::from_std(display_reader);
        let mut buffer = Vec::new();
        let bytes_read = timeout(
            ready_timeout,
            BufReader::new(reader).read_until(b'\n', &mut buffer),
        )
        .await
        .map_err(|_| {
            ComputerUseError::new(
                ComputerUseErrorCode::SessionStartFailed,
                "Xvfb did not report a display before the startup timeout",
                /*retryable*/ true,
            )
        })?
        .map_err(session_start_failed)?;

        if bytes_read == 0 {
            return Err(ComputerUseError::new(
                ComputerUseErrorCode::SessionStartFailed,
                "Xvfb exited before reporting a display number",
                /*retryable*/ true,
            ));
        }

        let display_number = String::from_utf8(buffer).map_err(|error| {
            ComputerUseError::new(
                ComputerUseErrorCode::SessionStartFailed,
                format!("Xvfb reported a non-UTF-8 display number: {error}"),
                /*retryable*/ true,
            )
        })?;
        let display_number = display_number.trim();
        if display_number.is_empty() || !display_number.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(ComputerUseError::new(
                ComputerUseErrorCode::SessionStartFailed,
                format!("Xvfb reported an invalid display number: {display_number:?}"),
                /*retryable*/ true,
            ));
        }

        Ok(format!(":{display_number}"))
    }

    fn spawn_xvfb(
        program: &Path,
        session_dir: &Path,
        xauthority_path: &Path,
        displayfd: i32,
    ) -> Result<OwnedChild, ComputerUseError> {
        let mut command = Command::new(program);
        command
            .kill_on_drop(true)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(session_dir)
            .arg("-displayfd")
            .arg(displayfd.to_string())
            .arg("-screen")
            .arg("0")
            .arg(format!(
                "{SCREENSHOT_VIEWPORT_WIDTH}x{SCREENSHOT_VIEWPORT_HEIGHT}x{SCREENSHOT_VIEWPORT_DEPTH}"
            ))
            .arg("-dpi")
            .arg(SCREENSHOT_VIEWPORT_DPI.to_string())
            .arg("-nolisten")
            .arg("tcp")
            .arg("-auth")
            .arg(xauthority_path);
        let parent_pid = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                codex_utils_pty::process_group::set_parent_death_signal(parent_pid)
            });
        }

        let child = command
            .spawn()
            .map_err(|error| prerequisite_or_start_error("Xvfb", error))?;
        OwnedChild::new(child)
    }

    fn spawn_openbox(
        program: &Path,
        session_dir: &Path,
        environment: &DesktopEnvironment,
    ) -> Result<OwnedChild, ComputerUseError> {
        let mut command = Command::new(program);
        command
            .kill_on_drop(true)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(session_dir)
            .env("DISPLAY", &environment.display)
            .env("XAUTHORITY", &environment.xauthority);
        let parent_pid = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                codex_utils_pty::process_group::set_parent_death_signal(parent_pid)
            });
        }

        let child = command
            .spawn()
            .map_err(|error| prerequisite_or_start_error("Openbox", error))?;
        OwnedChild::new(child)
    }

    fn prerequisite_or_start_error(label: &str, error: io::Error) -> ComputerUseError {
        match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => ComputerUseError::new(
                ComputerUseErrorCode::PrerequisiteMissing,
                format!("{label} is unavailable: {error}"),
                /*retryable*/ false,
            ),
            _ => session_start_failed(error),
        }
    }

    fn session_start_failed(error: io::Error) -> ComputerUseError {
        ComputerUseError::new(
            ComputerUseErrorCode::SessionStartFailed,
            format!("failed to start the Computer Use session: {error}"),
            /*retryable*/ true,
        )
    }

    fn internal_error(error: io::Error) -> ComputerUseError {
        ComputerUseError::new(
            ComputerUseErrorCode::InternalError,
            format!("failed to stop the Computer Use session cleanly: {error}"),
            /*retryable*/ true,
        )
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod imp {
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::protocol::ComputerUseError;
    use crate::protocol::ComputerUseErrorCode;
    use crate::protocol::DesktopEnvironment;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DesktopCommandPaths {
        pub xvfb: PathBuf,
        pub openbox: PathBuf,
    }

    impl Default for DesktopCommandPaths {
        fn default() -> Self {
            Self {
                xvfb: PathBuf::from("Xvfb"),
                openbox: PathBuf::from("openbox"),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DesktopSessionConfig {
        pub command_paths: DesktopCommandPaths,
        pub temp_root: PathBuf,
        pub display_ready_timeout: Duration,
        pub shutdown_grace_period: Duration,
    }

    impl Default for DesktopSessionConfig {
        fn default() -> Self {
            Self {
                command_paths: DesktopCommandPaths::default(),
                temp_root: std::env::temp_dir().join("codex-computer-use"),
                display_ready_timeout: Duration::from_secs(5),
                shutdown_grace_period: Duration::from_secs(2),
            }
        }
    }

    #[derive(Debug)]
    pub struct DesktopSession;

    impl DesktopSession {
        pub async fn start(_config: &DesktopSessionConfig) -> Result<Self, ComputerUseError> {
            Err(ComputerUseError::new(
                ComputerUseErrorCode::UnsupportedPlatform,
                "Computer Use requires x86_64 Linux",
                /*retryable*/ false,
            ))
        }

        pub fn session_id(&self) -> &str {
            ""
        }

        pub fn environment(&self) -> &DesktopEnvironment {
            unreachable!("unsupported platform stub has no session environment")
        }

        pub fn session_dir(&self) -> &Path {
            unreachable!("unsupported platform stub has no session directory")
        }

        pub fn xauthority_path(&self) -> &Path {
            unreachable!("unsupported platform stub has no Xauthority path")
        }

        pub fn sky_output_dir(&self) -> &Path {
            unreachable!("unsupported platform stub has no sky output directory")
        }

        pub async fn ensure_healthy(&mut self) -> Result<(), ComputerUseError> {
            Err(ComputerUseError::new(
                ComputerUseErrorCode::UnsupportedPlatform,
                "Computer Use requires x86_64 Linux",
                /*retryable*/ false,
            ))
        }

        pub async fn stop(self) -> Result<String, ComputerUseError> {
            Err(ComputerUseError::new(
                ComputerUseErrorCode::UnsupportedPlatform,
                "Computer Use requires x86_64 Linux",
                /*retryable*/ false,
            ))
        }
    }
}

pub use imp::DesktopCommandPaths;
pub use imp::DesktopSession;
pub use imp::DesktopSessionConfig;
