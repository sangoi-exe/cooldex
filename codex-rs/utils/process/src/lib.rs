//! Process identity and signaling primitives shared by local runtime owners.

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;

#[cfg(unix)]
use tokio::process::Command;

/// A PID paired with the operating system's process-start identity.
///
/// Consumers must compare both fields before treating a PID as the process
/// they originally recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessIdentity {
    pid: u32,
    process_start_time: String,
}

impl ProcessIdentity {
    /// Creates an identity from a PID and an already captured start identity.
    pub fn from_parts(pid: u32, process_start_time: String) -> Result<Self> {
        if process_start_time.trim().is_empty() {
            bail!("process start identity must not be empty");
        }
        Ok(Self {
            pid,
            process_start_time,
        })
    }

    /// Captures the current operating-system identity for `pid`.
    pub async fn capture(pid: u32) -> Result<Self> {
        let process_start_time = read_process_start_time(pid).await?;
        Self::from_parts(pid, process_start_time)
    }

    /// Captures the identity of the current process.
    pub async fn current() -> Result<Self> {
        Self::capture(std::process::id()).await
    }

    /// Returns the recorded process ID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the recorded operating-system start identity.
    pub fn process_start_time(&self) -> &str {
        &self.process_start_time
    }

    /// Reports whether the recorded PID still denotes the same live process.
    pub async fn is_active(&self) -> Result<bool> {
        process_matches_identity(self).await
    }
}

/// A platform process signal supported by [`send_signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
/// Reports whether a process with `pid` currently exists.
fn process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
/// Always returns `false` because process probing is unsupported on this platform.
fn process_exists(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
/// Sends `signal` to `pid`, treating an already exited process as success.
pub fn send_signal(pid: u32, signal: ProcessSignal) -> Result<()> {
    let raw_pid =
        libc::pid_t::try_from(pid).with_context(|| format!("process pid {pid} is out of range"))?;
    let signal = match signal {
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    let result = unsafe { libc::kill(raw_pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).with_context(|| format!("failed to signal process {pid}"))
}

#[cfg(not(unix))]
/// Returns an unsupported-platform error.
pub fn send_signal(_pid: u32, _signal: ProcessSignal) -> Result<()> {
    bail!("process signaling is unsupported on this platform")
}

/// Reaps `pid` if it is an exited direct child of the current process.
#[cfg(unix)]
pub fn reap_exited_child(pid: u32) {
    if let Ok(raw_pid) = libc::pid_t::try_from(pid)
        && raw_pid > 0
    {
        unsafe { libc::waitpid(raw_pid, std::ptr::null_mut(), libc::WNOHANG) };
    }
}

#[cfg(not(unix))]
pub fn reap_exited_child(_pid: u32) {}

/// Arms Linux parent-death delivery with `SIGKILL` and closes the fork/exec
/// race by verifying the captured parent immediately afterward.
#[cfg(target_os = "linux")]
pub fn arm_parent_death_sigkill(parent_pid: libc::pid_t) -> std::io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } != parent_pid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "parent exited before the child armed its death signal",
        ));
    }
    Ok(())
}

#[cfg(unix)]
async fn process_matches_identity(identity: &ProcessIdentity) -> Result<bool> {
    if !process_exists(identity.pid) {
        return Ok(false);
    }
    match read_process_start_time(identity.pid).await {
        Ok(start_time) => Ok(start_time == identity.process_start_time),
        Err(_err) if !process_exists(identity.pid) => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(not(unix))]
async fn process_matches_identity(_identity: &ProcessIdentity) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
async fn read_process_start_time(pid: u32) -> Result<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .await
        .context("failed to invoke ps for process identity")?;
    if !output.status.success() {
        bail!("failed to read start identity for process {pid}");
    }
    let start_time =
        String::from_utf8(output.stdout).context("process start identity was not utf-8")?;
    let start_time = start_time.trim();
    if start_time.is_empty() {
        bail!("process {pid} has no start identity");
    }
    Ok(start_time.to_string())
}

#[cfg(not(unix))]
async fn read_process_start_time(_pid: u32) -> Result<String> {
    bail!("process identity is unsupported on this platform")
}

#[cfg(all(test, unix))]
#[path = "process_tests.rs"]
mod tests;
