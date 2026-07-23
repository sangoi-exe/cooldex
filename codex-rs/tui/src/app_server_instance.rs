use std::ffi::CString;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use codex_app_server_client::AppServerClient;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_config::ProfileV2Name;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_process::ProcessIdentity;
use codex_utils_process::ProcessSignal;
use codex_utils_process::arm_parent_death_sigkill;
use codex_utils_process::send_signal;
use color_eyre::eyre::Context;
use color_eyre::eyre::Result;
use color_eyre::eyre::eyre;
use serde::Deserialize;
use serde::Serialize;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio::time::timeout;
use tracing::warn;
use uuid::Uuid;

const INSTANCE_ROOT_MODE: u32 = 0o700;
const OWNER_FILE_MODE: u32 = 0o600;
const OWNER_RECORD_VERSION: u32 = 1;
const READINESS_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);
const TERMINATE_GRACE_PERIOD: Duration = Duration::from_secs(5);
const KILL_REAP_PERIOD: Duration = Duration::from_secs(5);
const OWNER_SUFFIX: &str = ".owner.json";

pub(crate) struct InstanceChildLaunch {
    pub(crate) codex_exe: PathBuf,
    pub(crate) codex_home: AbsolutePathBuf,
    pub(crate) raw_config_overrides: Vec<String>,
    pub(crate) profile: Option<ProfileV2Name>,
    pub(crate) strict_config: bool,
}

pub(crate) struct StartedInstanceChild {
    pub(crate) client: AppServerClient,
    pub(crate) supervisor: AppServerInstance,
}

pub(crate) struct AppServerInstance {
    child: Option<Child>,
    endpoint: RemoteAppServerEndpoint,
    owned_state: Option<OwnedInstanceState>,
}

impl AppServerInstance {
    pub(crate) async fn start(launch: InstanceChildLaunch) -> Result<StartedInstanceChild> {
        let parent = ProcessIdentity::current()
            .await
            .map_err(|err| eyre!("failed to capture TUI process identity: {err}"))?;
        let instance_root = launch.codex_home.join("tmp").join("app-server-instances");
        prepare_instance_root(instance_root.as_path())?;
        cleanup_orphans(instance_root.as_path()).await?;

        let owned_state = OwnedInstanceState::create(instance_root, parent.clone())?;
        let endpoint = RemoteAppServerEndpoint::UnixSocket {
            socket_path: owned_state.socket_path.clone(),
        };
        let stderr_file = match owned_state.open_stderr_log() {
            Ok(stderr_file) => stderr_file,
            Err(err) => {
                owned_state.remove_if_owned();
                return Err(err);
            }
        };
        let mut command = Command::new(&launch.codex_exe);
        for raw_override in &launch.raw_config_overrides {
            command.arg("-c").arg(raw_override);
        }
        if let Some(profile) = &launch.profile {
            command.arg("-p").arg(profile.as_str());
        }
        command
            .arg("app-server")
            .arg("--instance-child")
            .arg("--analytics-default-enabled")
            .arg("--listen")
            .arg(format!("unix://{}", owned_state.socket_path.display()));
        if launch.strict_config {
            command.arg("--strict-config");
        }
        command
            .env("CODEX_HOME", launch.codex_home.as_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file));
        #[cfg(test)]
        command.env(
            "CODEX_TEST_INSTANCE_ENDPOINT",
            owned_state.socket_path.as_path(),
        );

        let parent_pid = match libc::pid_t::try_from(parent.pid()) {
            Ok(parent_pid) => parent_pid,
            Err(err) => {
                owned_state.remove_if_owned();
                return Err(err).wrap_err("TUI process ID is out of range");
            }
        };
        unsafe {
            command.pre_exec(move || arm_parent_death_sigkill(parent_pid));
        }

        let child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                owned_state.remove_if_owned();
                return Err(err).wrap_err_with(|| {
                    format!(
                        "failed to spawn instance-owned app-server using {}",
                        launch.codex_exe.display()
                    )
                });
            }
        };
        let child_pid = child.id();
        let mut supervisor = Self {
            child: Some(child),
            endpoint: endpoint.clone(),
            owned_state: Some(owned_state),
        };

        #[cfg(test)]
        wait_for_spawn_publication_test_barrier(child_pid).await?;

        let startup = timeout(READINESS_TIMEOUT, async {
            let child_identity = ProcessIdentity::capture(child_pid).await.map_err(|err| {
                eyre!("failed to capture instance-owned app-server identity: {err}")
            })?;
            let owned_state = supervisor
                .owned_state
                .as_ref()
                .ok_or_else(|| eyre!("instance supervisor lost owned state during startup"))?;
            owned_state.publish(OwnerState::Spawned, Some(child_identity))?;
            let client = supervisor.wait_until_ready().await?;
            let owned_state = supervisor
                .owned_state
                .as_ref()
                .ok_or_else(|| eyre!("instance supervisor lost owned state after readiness"))?;
            owned_state.publish(OwnerState::Ready, supervisor.child_identity().await?)?;
            Ok::<_, color_eyre::Report>(client)
        })
        .await
        .unwrap_or_else(|_| Err(eyre!("timed out starting instance-owned app-server")));

        match startup {
            Ok(client) => Ok(StartedInstanceChild { client, supervisor }),
            Err(err) => {
                let shutdown_result = supervisor.shutdown().await;
                match shutdown_result {
                    Ok(()) => Err(err),
                    Err(shutdown_err) => Err(err.wrap_err(shutdown_err.to_string())),
                }
            }
        }
    }

    pub(crate) fn endpoint(&self) -> &RemoteAppServerEndpoint {
        &self.endpoint
    }

    pub(crate) async fn shutdown(self) -> std::io::Result<()> {
        self.shutdown_after_client(Duration::ZERO).await
    }

    pub(crate) async fn shutdown_after_client(
        mut self,
        client_shutdown_elapsed: Duration,
    ) -> std::io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Err(std::io::Error::other(
                "instance supervisor lost its child before shutdown",
            ));
        };
        let remaining_grace = SHUTDOWN_GRACE_PERIOD.saturating_sub(client_shutdown_elapsed);
        if wait_for_exit(child, remaining_grace).await?.is_none() {
            send_signal(child.id(), ProcessSignal::Terminate).map_err(std::io::Error::other)?;
        }
        if wait_for_exit(child, TERMINATE_GRACE_PERIOD)
            .await?
            .is_none()
        {
            send_signal(child.id(), ProcessSignal::Kill).map_err(std::io::Error::other)?;
        }
        if wait_for_exit(child, KILL_REAP_PERIOD).await?.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "timed out reaping instance-owned app-server child {}",
                    child.id()
                ),
            ));
        }
        self.child = None;
        if let Some(owned_state) = self.owned_state.take() {
            owned_state.remove_if_owned();
        }
        Ok(())
    }

    async fn wait_until_ready(&mut self) -> Result<AppServerClient> {
        let deadline = Instant::now() + READINESS_TIMEOUT;
        let mut last_connect_error = None;
        loop {
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| eyre!("instance supervisor lost its child during readiness"))?;
            if let Some(status) = child
                .try_wait()
                .wrap_err("failed to inspect instance-owned app-server child")?
            {
                return Err(eyre!(
                    "instance-owned app-server exited before readiness with {status}"
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let detail = last_connect_error
                    .map(|err: std::io::Error| format!(": {err}"))
                    .unwrap_or_default();
                return Err(eyre!(
                    "timed out waiting for instance-owned app-server readiness{detail}"
                ));
            }
            match timeout(remaining, connect(self.endpoint.clone())).await {
                Ok(Ok(client)) => {
                    let child = self.child.as_mut().ok_or_else(|| {
                        eyre!("instance supervisor lost its child after readiness")
                    })?;
                    if let Some(status) = child
                        .try_wait()
                        .wrap_err("failed to confirm instance-owned app-server liveness")?
                    {
                        return Err(eyre!(
                            "instance-owned app-server exited during readiness with {status}"
                        ));
                    }
                    return Ok(client);
                }
                Ok(Err(err)) => last_connect_error = Some(err),
                Err(_) => {
                    return Err(eyre!(
                        "timed out during instance-owned app-server initialize handshake"
                    ));
                }
            }
            sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))).await;
        }
    }

    async fn child_identity(&self) -> Result<Option<ProcessIdentity>> {
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| eyre!("instance supervisor lost its child before publication"))?;
        Ok(Some(ProcessIdentity::capture(child.id()).await.map_err(
            |err| eyre!("failed to recapture ready app-server child identity: {err}"),
        )?))
    }
}

impl Drop for AppServerInstance {
    fn drop(&mut self) {
        let mut child_stopped = true;
        if let Some(mut child) = self.child.take() {
            child_stopped = match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => child.kill().and_then(|()| child.wait()).is_ok(),
                Err(_) => false,
            };
        }
        if child_stopped && let Some(owned_state) = self.owned_state.take() {
            owned_state.remove_if_owned();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OwnerState {
    Preparing,
    Spawned,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnerRecord {
    version: u32,
    nonce: String,
    state: OwnerState,
    parent: ProcessIdentity,
    child: Option<ProcessIdentity>,
}

struct OwnedInstanceState {
    root: AbsolutePathBuf,
    nonce: String,
    owner_path: PathBuf,
    instance_dir: PathBuf,
    socket_path: AbsolutePathBuf,
    parent: ProcessIdentity,
}

impl OwnedInstanceState {
    fn create(root: AbsolutePathBuf, parent: ProcessIdentity) -> Result<Self> {
        let nonce = Uuid::new_v4().to_string();
        let owner_path = root.join(format!("{nonce}{OWNER_SUFFIX}"));
        let instance_dir = root.join(&nonce);
        let socket_path = instance_dir.join("app-server.sock");
        let owned = Self {
            root,
            nonce,
            owner_path: owner_path.into_path_buf(),
            instance_dir: instance_dir.into_path_buf(),
            socket_path,
            parent,
        };
        owned.publish(OwnerState::Preparing, None)?;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(INSTANCE_ROOT_MODE);
        if let Err(err) = builder.create(&owned.instance_dir) {
            owned.remove_if_owned();
            return Err(err).wrap_err_with(|| {
                format!(
                    "failed to create private app-server directory {}",
                    owned.instance_dir.display()
                )
            });
        }
        if let Err(err) = std::fs::set_permissions(
            &owned.instance_dir,
            std::fs::Permissions::from_mode(INSTANCE_ROOT_MODE),
        ) {
            owned.remove_if_owned();
            return Err(err).wrap_err("failed to secure private app-server directory");
        }
        if let Err(err) = sync_directory(owned.root.as_path()) {
            owned.remove_if_owned();
            return Err(err);
        }
        Ok(owned)
    }

    fn open_stderr_log(&self) -> Result<File> {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(OWNER_FILE_MODE)
            .open(self.instance_dir.join("app-server.stderr.log"))
            .wrap_err("failed to create instance-owned app-server stderr log")
    }

    fn publish(&self, state: OwnerState, child: Option<ProcessIdentity>) -> Result<()> {
        let record = OwnerRecord {
            version: OWNER_RECORD_VERSION,
            nonce: self.nonce.clone(),
            state,
            parent: self.parent.clone(),
            child,
        };
        validate_record(&record, &self.nonce)?;
        let bytes = serde_json::to_vec(&record).wrap_err("failed to serialize owner record")?;
        if state == OwnerState::Preparing {
            write_new_file(&self.owner_path, &bytes)?;
        } else {
            let temp_path = self
                .instance_dir
                .join(format!("owner.tmp-{}.json", Uuid::new_v4()));
            write_new_file(&temp_path, &bytes)?;
            if let Err(err) = std::fs::rename(&temp_path, &self.owner_path) {
                let _ = std::fs::remove_file(&temp_path);
                return Err(err).wrap_err_with(|| {
                    format!(
                        "failed to replace owner record {}",
                        self.owner_path.display()
                    )
                });
            }
            sync_directory(self.root.as_path())?;
        }
        Ok(())
    }

    fn remove_if_owned(&self) {
        let owned = std::fs::read(&self.owner_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<OwnerRecord>(&bytes).ok())
            .is_some_and(|record| {
                validate_record(&record, &self.nonce).is_ok() && record.parent == self.parent
            });
        if !owned {
            return;
        }
        let instance_removed = match self.instance_dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_dir() => {
                std::fs::remove_dir_all(&self.instance_dir).is_ok()
            }
            Ok(_) => false,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        };
        if instance_removed && std::fs::remove_file(&self.owner_path).is_ok() {
            let _ = sync_directory(self.root.as_path());
        }
    }
}

async fn connect(endpoint: RemoteAppServerEndpoint) -> std::io::Result<AppServerClient> {
    RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint,
        client_name: "codex-tui".to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: crate::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await
    .map(AppServerClient::Remote)
}

async fn wait_for_exit(
    child: &mut Child,
    wait: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + wait;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

fn prepare_instance_root(root: &Path) -> Result<()> {
    if let Ok(metadata) = root.symlink_metadata()
        && !metadata.file_type().is_dir()
    {
        return Err(eyre!(
            "instance app-server root is not a directory: {}",
            root.display()
        ));
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(INSTANCE_ROOT_MODE);
    builder.create(root).wrap_err_with(|| {
        format!(
            "failed to create instance app-server root {}",
            root.display()
        )
    })?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(INSTANCE_ROOT_MODE))
        .wrap_err("failed to secure instance app-server root")?;
    Ok(())
}

async fn cleanup_orphans(root: &Path) -> Result<()> {
    let entries = std::fs::read_dir(root)
        .wrap_err_with(|| format!("failed to scan instance app-server root {}", root.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .collect::<Vec<_>>();
    for entry in entries {
        let Some(nonce) = nonce_from_owner_filename(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let bytes = match std::fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warn!(path = %entry.path().display(), %err, "failed to inspect instance owner record");
                continue;
            }
        };
        let record = match serde_json::from_slice::<OwnerRecord>(&bytes) {
            Ok(record) if validate_record(&record, &nonce).is_ok() => record,
            _ => continue,
        };
        let removable = match record_is_removable(&record).await {
            Ok(removable) => removable,
            Err(err) => {
                warn!(path = %entry.path().display(), %err, "could not prove instance owner record orphaned");
                continue;
            }
        };
        if !removable {
            continue;
        }
        let instance_dir = root.join(&nonce);
        match instance_dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => continue,
        }
        let claim_path = root.join(format!("{nonce}.claim-{}.json", Uuid::new_v4()));
        match rename_noreplace(&entry.path(), &claim_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warn!(path = %entry.path().display(), %err, "failed to claim orphan owner record");
                continue;
            }
        }
        let claimed_bytes = match std::fs::read(&claim_path) {
            Ok(claimed_bytes) if claimed_bytes == bytes => claimed_bytes,
            _ => continue,
        };
        let claimed_record = match serde_json::from_slice::<OwnerRecord>(&claimed_bytes) {
            Ok(record) if validate_record(&record, &nonce).is_ok() => record,
            _ => continue,
        };
        let still_removable = match record_is_removable(&claimed_record).await {
            Ok(removable) => removable,
            Err(err) => {
                warn!(path = %claim_path.display(), %err, "could not revalidate claimed instance orphan");
                continue;
            }
        };
        if !still_removable {
            continue;
        }
        match instance_dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_dir() => {
                if std::fs::remove_dir_all(&instance_dir).is_err() {
                    continue;
                }
            }
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => continue,
        }
        let _ = std::fs::remove_file(&claim_path);
        let _ = sync_directory(root);
    }
    Ok(())
}

fn validate_record(record: &OwnerRecord, filename_nonce: &str) -> Result<()> {
    let nonce = Uuid::parse_str(&record.nonce).wrap_err("owner record nonce is invalid")?;
    if nonce.to_string() != record.nonce || record.nonce != filename_nonce {
        return Err(eyre!("owner record nonce does not match its filename"));
    }
    if record.version != OWNER_RECORD_VERSION
        || record.parent.pid() == 0
        || record.parent.process_start_time().trim().is_empty()
    {
        return Err(eyre!("owner record version or parent identity is invalid"));
    }
    match (record.state, record.child.as_ref()) {
        (OwnerState::Preparing, None) => Ok(()),
        (OwnerState::Spawned | OwnerState::Ready, Some(child))
            if child.pid() != 0 && !child.process_start_time().trim().is_empty() =>
        {
            Ok(())
        }
        _ => Err(eyre!("owner record state and child identity disagree")),
    }
}

fn nonce_from_owner_filename(filename: &str) -> Option<String> {
    let nonce = filename.strip_suffix(OWNER_SUFFIX).or_else(|| {
        let (nonce, claim) = filename.split_once(".claim-")?;
        let claim_id = claim.strip_suffix(".json")?;
        let parsed_claim = Uuid::parse_str(claim_id).ok()?;
        if parsed_claim.to_string() != claim_id {
            return None;
        }
        Some(nonce)
    })?;
    let parsed = Uuid::parse_str(nonce).ok()?;
    (parsed.to_string() == nonce).then(|| nonce.to_string())
}

async fn record_is_removable(record: &OwnerRecord) -> Result<bool> {
    if record
        .parent
        .is_active()
        .await
        .map_err(|err| eyre!("failed to inspect recorded parent identity: {err}"))?
    {
        return Ok(false);
    }
    match record.state {
        OwnerState::Preparing => Ok(true),
        OwnerState::Spawned | OwnerState::Ready => {
            let Some(child) = record.child.as_ref() else {
                return Err(eyre!(
                    "validated spawned or ready owner record lacks a child identity"
                ));
            };
            Ok(!child
                .is_active()
                .await
                .map_err(|err| eyre!("failed to inspect recorded child identity: {err}"))?)
        }
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(OWNER_FILE_MODE)
        .open(path)
        .wrap_err_with(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)
        .wrap_err_with(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .wrap_err_with(|| format!("failed to sync {}", path.display()))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .wrap_err_with(|| format!("failed to sync directory {}", path.display()))
}

fn rename_noreplace(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid source path")
    })?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid target path")
    })?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
async fn wait_for_spawn_publication_test_barrier(child_pid: u32) -> Result<()> {
    let Some(ready_path) = std::env::var_os("CODEX_TEST_INSTANCE_SPAWN_READY").map(PathBuf::from)
    else {
        return Ok(());
    };
    let release_path = std::env::var_os("CODEX_TEST_INSTANCE_SPAWN_RELEASE")
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("spawn publication test barrier is missing its release path"))?;
    std::fs::write(&ready_path, child_pid.to_string())
        .wrap_err("failed to publish spawn-barrier child PID")?;
    while !release_path.try_exists().unwrap_or(false) {
        sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(test)]
#[path = "app_server_instance_tests.rs"]
mod tests;
