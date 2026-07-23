use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use codex_app_server_client::RemoteAppServerEndpoint;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cargo_bin::cargo_bin;
use codex_utils_process::ProcessIdentity;
use codex_utils_process::ProcessSignal;
use codex_utils_process::send_signal;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use pretty_assertions::assert_eq;
use serial_test::serial;
use tempfile::TempDir;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio::time::timeout;

use super::*;

const PARENT_HELPER_MODE_ENV: &str = "CODEX_TEST_INSTANCE_PARENT_HELPER_MODE";
const PARENT_HELPER_HOME_ENV: &str = "CODEX_TEST_INSTANCE_PARENT_HELPER_HOME";
const PARENT_HELPER_READY_ENV: &str = "CODEX_TEST_INSTANCE_PARENT_HELPER_READY";
const SIGNAL_HELPER_READY_ENV: &str = "CODEX_TEST_INSTANCE_SIGNAL_HELPER_READY";
const SIGNAL_HELPER_IGNORE_TERM_ENV: &str = "CODEX_TEST_INSTANCE_SIGNAL_HELPER_IGNORE_TERM";
const TEST_INSTANCE_ENDPOINT_ENV: &str = "CODEX_TEST_INSTANCE_ENDPOINT";

fn absolute(path: &Path) -> Result<AbsolutePathBuf> {
    AbsolutePathBuf::from_absolute_path(path).map_err(color_eyre::Report::new)
}

fn dead_identity(label: &str) -> ProcessIdentity {
    ProcessIdentity::from_parts(u32::MAX, label.to_string()).expect("valid dead identity")
}

async fn identity_is_active(identity: &ProcessIdentity) -> Result<bool> {
    identity
        .is_active()
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to inspect process identity: {err}"))
}

fn owner_record(
    nonce: &str,
    state: OwnerState,
    parent: ProcessIdentity,
    child: Option<ProcessIdentity>,
) -> OwnerRecord {
    OwnerRecord {
        version: OWNER_RECORD_VERSION,
        nonce: nonce.to_string(),
        state,
        parent,
        child,
    }
}

fn write_record(path: &Path, record: &OwnerRecord) -> Result<()> {
    let bytes = serde_json::to_vec(record)?;
    write_new_file(path, &bytes)
}

fn create_instance_dir(root: &Path, nonce: &str) -> Result<PathBuf> {
    let path = root.join(nonce);
    std::fs::create_dir(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(INSTANCE_ROOT_MODE))?;
    Ok(path)
}

fn owner_entries(root: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = std::fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

fn write_instance_config(home: &Path) -> Result<()> {
    std::fs::write(
        home.join("config.toml"),
        "[tui]\napp_server_mode = \"instance_child\"\n",
    )?;
    Ok(())
}

fn instance_launch(home: &Path) -> Result<InstanceChildLaunch> {
    let codex_exe = if codex_utils_cargo_bin::runfiles_available() {
        cargo_bin("codex")?
    } else {
        let test_exe = std::env::current_exe()?;
        let test_exe = test_exe.to_string_lossy();
        let quoted_test_exe = shlex::try_quote(test_exe.as_ref())?;
        let wrapper = home.join(format!("codex-test-wrapper-{}.sh", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&wrapper)?;
        writeln!(
            file,
            "#!/bin/sh\nexec {quoted_test_exe} --exact app_server_instance::tests::app_server_child_helper --ignored --nocapture"
        )?;
        file.sync_all()?;
        wrapper
    };
    Ok(InstanceChildLaunch {
        codex_exe,
        codex_home: absolute(home)?,
        raw_config_overrides: Vec::new(),
        profile: None,
        strict_config: false,
    })
}

#[tokio::test]
#[ignore]
async fn app_server_child_helper() -> Result<()> {
    let Some(socket_path) = std::env::var_os(TEST_INSTANCE_ENDPOINT_ENV).map(PathBuf::from) else {
        return Ok(());
    };
    codex_app_server::run_main_with_transport_options(
        codex_arg0::Arg0DispatchPaths {
            codex_self_exe: Some(std::env::current_exe()?),
            codex_linux_sandbox_exe: None,
            main_execve_wrapper_exe: None,
        },
        codex_utils_cli::CliConfigOverrides::default(),
        codex_config::LoaderOverrides::default(),
        /*strict_config*/ false,
        /*default_analytics_enabled*/ false,
        codex_app_server::AppServerTransport::UnixSocket {
            socket_path: absolute(&socket_path)?,
        },
        codex_protocol::protocol::SessionSource::Cli,
        codex_app_server::AppServerWebsocketAuthSettings::default(),
        codex_app_server::AppServerRuntimeOptions {
            plugin_startup_tasks: codex_app_server::PluginStartupTasks::Skip,
            remote_control_startup_mode:
                codex_app_server::RemoteControlStartupMode::DisabledEphemeral,
            install_shutdown_signal_handler: false,
            launch_mode: codex_app_server::AppServerLaunchMode::InstanceChild,
        },
    )
    .await
    .map_err(color_eyre::Report::new)
}

fn endpoint_socket(endpoint: &RemoteAppServerEndpoint) -> &AbsolutePathBuf {
    let RemoteAppServerEndpoint::UnixSocket { socket_path } = endpoint else {
        panic!("instance child should use a Unix socket");
    };
    socket_path
}

async fn wait_for_file(path: &Path, wait: Duration) -> Result<()> {
    timeout(wait, async {
        while !path.try_exists().unwrap_or(false) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .wrap_err_with(|| format!("timed out waiting for {}", path.display()))?;
    Ok(())
}

async fn wait_for_process_exit(identity: &ProcessIdentity, wait: Duration) -> Result<()> {
    timeout(wait, async {
        while identity_is_active(identity).await? {
            sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, color_eyre::Report>(())
    })
    .await
    .wrap_err_with(|| format!("timed out waiting for process {} to exit", identity.pid()))??;
    Ok(())
}

#[tokio::test]
async fn owner_record_validation_is_strict_and_state_aware() -> Result<()> {
    let parent = ProcessIdentity::current()
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture test identity: {err}"))?;
    let nonce = Uuid::new_v4().to_string();
    let preparing = owner_record(&nonce, OwnerState::Preparing, parent.clone(), None);
    validate_record(&preparing, &nonce)?;

    let serialized = serde_json::to_value(&preparing)?;
    assert_eq!(
        serialized.get("processStartTime"),
        None,
        "process identity should remain nested"
    );
    assert!(serialized["parent"].get("processStartTime").is_some());

    let ready_without_child = owner_record(&nonce, OwnerState::Ready, parent.clone(), None);
    assert!(validate_record(&ready_without_child, &nonce).is_err());
    assert!(validate_record(&preparing, &Uuid::new_v4().to_string()).is_err());

    let mut with_unknown = serde_json::to_value(&preparing)?;
    with_unknown["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<OwnerRecord>(with_unknown).is_err());

    let noncanonical_nonce = nonce.to_uppercase();
    let noncanonical = owner_record(&noncanonical_nonce, OwnerState::Preparing, parent, None);
    assert!(validate_record(&noncanonical, &noncanonical_nonce).is_err());
    Ok(())
}

#[tokio::test]
async fn owned_state_uses_private_modes_and_publishes_preparing_first() -> Result<()> {
    let home = TempDir::new()?;
    let root = absolute(&home.path().join("instances"))?;
    prepare_instance_root(root.as_path())?;
    let parent = ProcessIdentity::current()
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture test identity: {err}"))?;

    let owned = OwnedInstanceState::create(root.clone(), parent.clone())?;
    assert_eq!(
        std::fs::metadata(root.as_path())?.permissions().mode() & 0o777,
        INSTANCE_ROOT_MODE
    );
    assert_eq!(
        std::fs::metadata(&owned.instance_dir)?.permissions().mode() & 0o777,
        INSTANCE_ROOT_MODE
    );
    assert_eq!(
        std::fs::metadata(&owned.owner_path)?.permissions().mode() & 0o777,
        OWNER_FILE_MODE
    );
    let record: OwnerRecord = serde_json::from_slice(&std::fs::read(&owned.owner_path)?)?;
    assert_eq!(
        record,
        owner_record(&owned.nonce, OwnerState::Preparing, parent, None)
    );
    owned.remove_if_owned();
    assert!(owner_entries(root.as_path())?.is_empty());
    Ok(())
}

#[tokio::test]
#[serial(app_server_instance)]
async fn orphan_cleanup_removes_only_proven_dead_owned_state() -> Result<()> {
    let home = TempDir::new()?;
    let root = home.path().join("instances");
    prepare_instance_root(&root)?;
    let mut live_process = tokio::process::Command::new("/bin/sleep")
        .arg("300")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let live_identity = ProcessIdentity::capture(live_process.id().expect("live process PID"))
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture live identity: {err}"))?;
    let dead_parent = dead_identity("dead-parent");
    let dead_child = dead_identity("dead-child");

    let preparing_nonce = Uuid::new_v4().to_string();
    write_record(
        &root.join(format!("{preparing_nonce}{OWNER_SUFFIX}")),
        &owner_record(
            &preparing_nonce,
            OwnerState::Preparing,
            dead_parent.clone(),
            None,
        ),
    )?;

    let spawned_nonce = Uuid::new_v4().to_string();
    create_instance_dir(&root, &spawned_nonce)?;
    write_record(
        &root.join(format!("{spawned_nonce}{OWNER_SUFFIX}")),
        &owner_record(
            &spawned_nonce,
            OwnerState::Spawned,
            dead_parent.clone(),
            Some(dead_child.clone()),
        ),
    )?;

    let claim_nonce = Uuid::new_v4().to_string();
    create_instance_dir(&root, &claim_nonce)?;
    write_record(
        &root.join(format!("{claim_nonce}.claim-{}.json", Uuid::new_v4())),
        &owner_record(
            &claim_nonce,
            OwnerState::Ready,
            dead_parent.clone(),
            Some(dead_child.clone()),
        ),
    )?;

    let live_nonce = Uuid::new_v4().to_string();
    write_record(
        &root.join(format!("{live_nonce}{OWNER_SUFFIX}")),
        &owner_record(
            &live_nonce,
            OwnerState::Preparing,
            live_identity.clone(),
            None,
        ),
    )?;

    let live_child_nonce = Uuid::new_v4().to_string();
    create_instance_dir(&root, &live_child_nonce)?;
    write_record(
        &root.join(format!("{live_child_nonce}{OWNER_SUFFIX}")),
        &owner_record(
            &live_child_nonce,
            OwnerState::Ready,
            dead_parent.clone(),
            Some(live_identity),
        ),
    )?;

    let malformed_nonce = Uuid::new_v4().to_string();
    create_instance_dir(&root, &malformed_nonce)?;
    write_new_file(
        &root.join(format!("{malformed_nonce}{OWNER_SUFFIX}")),
        br#"{"version":1}"#,
    )?;

    let symlink_nonce = Uuid::new_v4().to_string();
    let foreign_target = home.path().join("foreign-target");
    std::fs::create_dir(&foreign_target)?;
    symlink(&foreign_target, root.join(&symlink_nonce))?;
    write_record(
        &root.join(format!("{symlink_nonce}{OWNER_SUFFIX}")),
        &owner_record(
            &symlink_nonce,
            OwnerState::Ready,
            dead_parent,
            Some(dead_child),
        ),
    )?;

    let foreign_dir = root.join("foreign-directory");
    std::fs::create_dir(&foreign_dir)?;

    cleanup_orphans(&root).await?;

    assert!(
        !root
            .join(format!("{preparing_nonce}{OWNER_SUFFIX}"))
            .exists()
    );
    assert!(!root.join(format!("{spawned_nonce}{OWNER_SUFFIX}")).exists());
    assert!(!root.join(&spawned_nonce).exists());
    assert!(!root.join(&claim_nonce).exists());
    assert!(root.join(format!("{live_nonce}{OWNER_SUFFIX}")).exists());
    assert!(
        root.join(format!("{live_child_nonce}{OWNER_SUFFIX}"))
            .exists()
    );
    assert!(root.join(&live_child_nonce).exists());
    assert!(
        root.join(format!("{malformed_nonce}{OWNER_SUFFIX}"))
            .exists()
    );
    assert!(root.join(&malformed_nonce).exists());
    assert!(root.join(format!("{symlink_nonce}{OWNER_SUFFIX}")).exists());
    assert!(
        root.join(&symlink_nonce)
            .symlink_metadata()?
            .file_type()
            .is_symlink()
    );
    assert!(foreign_dir.exists());
    assert!(foreign_target.exists());
    live_process.kill().await?;
    live_process.wait().await?;
    Ok(())
}

#[test]
fn rename_noreplace_does_not_overwrite_a_racing_claim() -> Result<()> {
    let home = TempDir::new()?;
    let source = home.path().join("source");
    let target = home.path().join("target");
    std::fs::write(&source, "source")?;
    std::fs::write(&target, "target")?;

    assert!(rename_noreplace(&source, &target).is_err());
    assert_eq!(std::fs::read_to_string(source)?, "source");
    assert_eq!(std::fs::read_to_string(target)?, "target");
    Ok(())
}

#[tokio::test]
#[serial(app_server_instance)]
async fn two_instance_children_use_distinct_endpoints_and_reap_cleanly() -> Result<()> {
    let home = TempDir::new()?;
    write_instance_config(home.path())?;
    let first_launch = instance_launch(home.path())?;
    let second_launch = instance_launch(home.path())?;
    let (first, second) = tokio::join!(
        AppServerInstance::start(first_launch),
        AppServerInstance::start(second_launch)
    );
    let first = first?;
    let second = second?;

    assert_ne!(first.supervisor.endpoint(), second.supervisor.endpoint());
    assert!(
        endpoint_socket(first.supervisor.endpoint())
            .as_path()
            .exists()
    );
    assert!(
        endpoint_socket(second.supervisor.endpoint())
            .as_path()
            .exists()
    );
    let first_identity = first
        .supervisor
        .child_identity()
        .await?
        .expect("first child identity");
    let second_identity = second
        .supervisor
        .child_identity()
        .await?
        .expect("second child identity");

    let first_session = crate::app_server_session::AppServerSession::new_instance_child(
        first.client,
        first.supervisor,
    );
    let second_session = crate::app_server_session::AppServerSession::new_instance_child(
        second.client,
        second.supervisor,
    );
    assert!(first_session.is_instance_child());
    assert!(second_session.is_instance_child());
    first_session.shutdown().await?;
    assert!(!identity_is_active(&first_identity).await?);
    assert!(identity_is_active(&second_identity).await?);
    second_session.shutdown().await?;
    assert!(!identity_is_active(&second_identity).await?);

    let root = home.path().join("tmp/app-server-instances");
    assert!(owner_entries(&root)?.is_empty());
    Ok(())
}

#[tokio::test]
#[serial(app_server_instance)]
async fn spawn_failure_and_early_exit_leave_no_owned_state() -> Result<()> {
    for codex_exe in [
        PathBuf::from("/definitely/missing/codex"),
        PathBuf::from("/bin/false"),
    ] {
        let home = TempDir::new()?;
        let launch = InstanceChildLaunch {
            codex_exe,
            codex_home: absolute(home.path())?,
            raw_config_overrides: Vec::new(),
            profile: None,
            strict_config: false,
        };
        assert!(AppServerInstance::start(launch).await.is_err());
        let root = home.path().join("tmp/app-server-instances");
        assert!(root.exists());
        assert!(owner_entries(&root)?.is_empty());
    }
    Ok(())
}

#[tokio::test]
#[serial(app_server_instance)]
async fn initialize_handshake_timeout_is_bounded_and_reaped() -> Result<()> {
    let home = TempDir::new()?;
    let socket_path = absolute(&home.path().join("unresponsive.sock"))?;
    let listener = tokio::net::UnixListener::bind(socket_path.as_path())?;
    let accept_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _stream = stream;
            sleep(Duration::from_secs(20)).await;
        }
    });
    let child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let child_pid = child.id();
    let child_identity = ProcessIdentity::capture(child_pid)
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture child identity: {err}"))?;
    let mut supervisor = AppServerInstance {
        child: Some(child),
        endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
        owned_state: None,
    };

    let started_at = Instant::now();
    let err = match supervisor.wait_until_ready().await {
        Ok(_) => panic!("unresponsive initialize handshake should time out"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("timed out"));
    assert!(started_at.elapsed() <= READINESS_TIMEOUT + Duration::from_secs(1));
    accept_task.abort();
    supervisor
        .shutdown_after_client(SHUTDOWN_GRACE_PERIOD)
        .await?;
    assert!(!identity_is_active(&child_identity).await?);
    Ok(())
}

#[tokio::test]
#[serial(app_server_instance)]
async fn unexpected_child_death_closes_the_remote_event_stream() -> Result<()> {
    let home = TempDir::new()?;
    write_instance_config(home.path())?;
    let mut started = AppServerInstance::start(instance_launch(home.path())?).await?;
    let child_pid = started
        .supervisor
        .child
        .as_ref()
        .expect("supervisor child")
        .id();
    let child_identity = ProcessIdentity::capture(child_pid)
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture child identity: {err}"))?;
    send_signal(child_pid, ProcessSignal::Kill)
        .map_err(|err| color_eyre::eyre::eyre!("failed to kill child: {err}"))?;

    timeout(Duration::from_secs(5), async {
        while started.client.next_event().await.is_some() {}
    })
    .await
    .wrap_err("app-server event stream did not close after child death")?;
    started.supervisor.shutdown().await?;
    assert!(!identity_is_active(&child_identity).await?);
    Ok(())
}

fn spawn_signal_helper(ready_path: &Path, ignore_term: bool) -> Result<std::process::Child> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--exact")
        .arg("app_server_instance::tests::controlled_shutdown_signal_helper")
        .arg("--ignored")
        .arg("--nocapture")
        .env(SIGNAL_HELPER_READY_ENV, ready_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if ignore_term {
        command.env(SIGNAL_HELPER_IGNORE_TERM_ENV, "1");
    }
    command.spawn().map_err(color_eyre::Report::new)
}

#[tokio::test]
#[serial(app_server_instance)]
async fn controlled_shutdown_escalates_from_term_to_kill() -> Result<()> {
    let home = TempDir::new()?;
    let endpoint = RemoteAppServerEndpoint::UnixSocket {
        socket_path: absolute(&home.path().join("unused.sock"))?,
    };

    let term_ready = home.path().join("term-ready");
    let term_child = spawn_signal_helper(&term_ready, false)?;
    let term_pid = term_child.id();
    wait_for_file(&term_ready, Duration::from_secs(5)).await?;
    let term_identity = ProcessIdentity::capture(term_pid)
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture TERM child identity: {err}"))?;
    AppServerInstance {
        child: Some(term_child),
        endpoint: endpoint.clone(),
        owned_state: None,
    }
    .shutdown_after_client(SHUTDOWN_GRACE_PERIOD)
    .await?;
    assert!(!identity_is_active(&term_identity).await?);

    let kill_ready = home.path().join("kill-ready");
    let kill_child = spawn_signal_helper(&kill_ready, true)?;
    let kill_pid = kill_child.id();
    wait_for_file(&kill_ready, Duration::from_secs(5)).await?;
    let kill_identity = ProcessIdentity::capture(kill_pid)
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to capture KILL child identity: {err}"))?;
    let started_at = Instant::now();
    AppServerInstance {
        child: Some(kill_child),
        endpoint,
        owned_state: None,
    }
    .shutdown_after_client(SHUTDOWN_GRACE_PERIOD)
    .await?;
    assert!(started_at.elapsed() >= TERMINATE_GRACE_PERIOD);
    assert!(!identity_is_active(&kill_identity).await?);
    Ok(())
}

#[test]
#[ignore]
fn controlled_shutdown_signal_helper() -> Result<()> {
    let Some(ready_path) = std::env::var_os(SIGNAL_HELPER_READY_ENV).map(PathBuf::from) else {
        return Ok(());
    };
    if std::env::var_os(SIGNAL_HELPER_IGNORE_TERM_ENV).is_some() {
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    std::fs::write(ready_path, std::process::id().to_string())?;
    loop {
        std::thread::park_timeout(Duration::from_secs(60));
    }
}

fn spawn_parent_helper(
    mode: &str,
    home: &Path,
    ready_path: &Path,
    release_path: &Path,
) -> Result<std::process::Child> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--exact")
        .arg("app_server_instance::tests::parent_death_helper")
        .arg("--ignored")
        .arg("--nocapture")
        .env(PARENT_HELPER_MODE_ENV, mode)
        .env(PARENT_HELPER_HOME_ENV, home)
        .env(PARENT_HELPER_READY_ENV, ready_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if mode == "preparing" {
        command
            .env("CODEX_TEST_INSTANCE_SPAWN_READY", ready_path)
            .env("CODEX_TEST_INSTANCE_SPAWN_RELEASE", release_path);
    }
    command.spawn().map_err(color_eyre::Report::new)
}

#[tokio::test]
#[serial(app_server_instance)]
async fn parent_sigkill_kills_children_before_and_after_identity_publication() -> Result<()> {
    for mode in ["preparing", "ready"] {
        let home = TempDir::new()?;
        write_instance_config(home.path())?;
        let ready_path = home.path().join("parent-helper-ready");
        let release_path = home.path().join("never-release");
        let mut parent = spawn_parent_helper(mode, home.path(), &ready_path, &release_path)?;
        wait_for_file(&ready_path, Duration::from_secs(20)).await?;
        let child_pid: u32 = std::fs::read_to_string(&ready_path)?.trim().parse()?;
        let child_identity = ProcessIdentity::capture(child_pid)
            .await
            .map_err(|err| color_eyre::eyre::eyre!("failed to capture child identity: {err}"))?;

        let root = home.path().join("tmp/app-server-instances");
        let owner_path = owner_entries(&root)?
            .into_iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .expect("owner record should exist before parent death");
        let record: OwnerRecord = serde_json::from_slice(&std::fs::read(owner_path)?)?;
        assert_eq!(
            record.state,
            if mode == "preparing" {
                OwnerState::Preparing
            } else {
                OwnerState::Ready
            }
        );

        parent.kill()?;
        parent.wait()?;
        wait_for_process_exit(&child_identity, Duration::from_secs(5)).await?;
        cleanup_orphans(&root).await?;
        assert!(owner_entries(&root)?.is_empty());
    }
    Ok(())
}

#[tokio::test]
#[ignore]
async fn parent_death_helper() -> Result<()> {
    let Some(mode) = std::env::var_os(PARENT_HELPER_MODE_ENV) else {
        return Ok(());
    };
    let home = PathBuf::from(
        std::env::var_os(PARENT_HELPER_HOME_ENV).expect("parent helper home should be set"),
    );
    let ready_path = PathBuf::from(
        std::env::var_os(PARENT_HELPER_READY_ENV).expect("parent helper ready path should be set"),
    );
    let started = AppServerInstance::start(instance_launch(&home)?).await?;
    if mode == "ready" {
        let child_pid = started
            .supervisor
            .child
            .as_ref()
            .expect("parent helper child")
            .id();
        let mut ready = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(OWNER_FILE_MODE)
            .open(ready_path)?;
        write!(ready, "{child_pid}")?;
        ready.sync_all()?;
    }
    let _started = started;
    loop {
        sleep(Duration::from_secs(60)).await;
    }
}
