mod pid;
mod stderr_log;
#[cfg(windows)]
pub(crate) mod windows;

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use codex_install_context::LocalPackageLane;
use serde::Serialize;

pub(crate) use pid::PidBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    Pid,
}

// Merge-safety anchor: PID and update backends share CODEX_HOME while carrying the selected local package lane as their daemon owner.
#[derive(Debug, Clone)]
pub(crate) struct BackendPaths {
    pub(crate) codex_home: PathBuf,
    pub(crate) codex_bin: PathBuf,
    pub(crate) pid_file: PathBuf,
    pub(crate) update_pid_file: PathBuf,
    pub(crate) local_package_lane: Option<LocalPackageLane>,
    pub(crate) remote_control_enabled: bool,
    pub(crate) feature_overrides: BTreeMap<String, bool>,
}

pub(crate) fn pid_backend(paths: BackendPaths) -> PidBackend {
    let mut backend = PidBackend::new(
        paths.codex_bin,
        paths.pid_file,
        paths.remote_control_enabled,
    )
    .with_daemon_owner(paths.codex_home, paths.local_package_lane);
    backend.feature_overrides = paths.feature_overrides;
    backend
}

pub(crate) fn pid_update_loop_backend(paths: BackendPaths) -> PidBackend {
    PidBackend::new_update_loop(
        paths.codex_bin,
        paths.update_pid_file,
        /*restore_release*/ None,
    )
    .with_daemon_owner(paths.codex_home, paths.local_package_lane)
}

pub(crate) async fn append_stderr_log_tail_context(pid_file: &Path, context: &mut String) {
    match pid::read_stderr_log_tail(pid_file).await {
        Ok(Some(tail)) => tail.append_to_context(context),
        Ok(None) => {}
        Err(err) => {
            context.push_str(&format!(
                "\n\nFailed to read managed app-server stderr log: {err:#}"
            ));
        }
    }
}
