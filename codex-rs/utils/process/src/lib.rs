//! Process identity and signaling primitives shared by local runtime owners.

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;

/// A Linux PID paired with its boot ID and `/proc` start ticks.
///
/// Consumers must compare the PID, boot ID, and start ticks before treating a
/// process as the one they originally recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    pid: u32,
    boot_id: String,
    start_ticks: u64,
}

impl ProcessIdentity {
    /// Creates an identity from a PID, Linux boot ID, and `/proc` start ticks.
    pub fn from_parts(pid: u32, boot_id: String, start_ticks: u64) -> Result<Self> {
        if boot_id.trim().is_empty() {
            bail!("process boot ID must not be empty");
        }
        Ok(Self {
            pid,
            boot_id,
            start_ticks,
        })
    }

    /// Captures the current operating-system identity for `pid`.
    pub async fn capture(pid: u32) -> Result<Self> {
        let boot_id = read_boot_id().await?;
        let start_ticks = read_process_start_ticks(pid).await?;
        Self::from_parts(pid, boot_id, start_ticks)
    }

    /// Captures the identity of the current process.
    pub async fn current() -> Result<Self> {
        Self::capture(std::process::id()).await
    }

    /// Returns the recorded process ID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the Linux boot ID recorded for this process.
    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    /// Returns the `/proc` start ticks recorded for this process.
    pub fn start_ticks(&self) -> u64 {
        self.start_ticks
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

#[cfg(target_os = "linux")]
/// Reports whether a process with `pid` currently exists.
fn process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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

#[cfg(target_os = "linux")]
async fn process_matches_identity(identity: &ProcessIdentity) -> Result<bool> {
    // A different boot proves staleness before a reused PID is inspected.
    if read_boot_id().await? != identity.boot_id {
        return Ok(false);
    }
    if !process_exists(identity.pid) {
        return Ok(false);
    }
    match read_process_start_ticks(identity.pid).await {
        Ok(start_ticks) => Ok(start_ticks == identity.start_ticks),
        Err(_err) if !process_exists(identity.pid) => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(not(target_os = "linux"))]
async fn process_matches_identity(_identity: &ProcessIdentity) -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "linux")]
async fn read_boot_id() -> Result<String> {
    let boot_id = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .await
        .context("failed to read Linux boot ID")?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        bail!("Linux boot ID is empty");
    }
    Ok(boot_id.to_owned())
}

#[cfg(target_os = "linux")]
async fn read_process_start_ticks(pid: u32) -> Result<u64> {
    let stat = tokio::fs::read(format!("/proc/{pid}/stat"))
        .await
        .with_context(|| format!("failed to read process stat for pid {pid}"))?;
    parse_start_ticks(&stat)
}

#[cfg(target_os = "linux")]
fn parse_start_ticks(stat: &[u8]) -> Result<u64> {
    // comm (field 2) can contain spaces and closing parentheses. The final ')'
    // terminates it; splitting the whole line on whitespace miscounts fields.
    let end = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .context("process stat has no comm")?;
    let fields = std::str::from_utf8(&stat[end + 1..]).context("invalid process stat fields")?;
    let start_ticks = fields
        .split_whitespace()
        .nth(/*n*/ 19)
        .context("process stat has no start time")?
        .parse()
        .context("process stat start time is invalid")?;
    Ok(start_ticks)
}

#[cfg(not(target_os = "linux"))]
async fn read_boot_id() -> Result<String> {
    bail!("process identity is unsupported on this platform")
}

#[cfg(not(target_os = "linux"))]
async fn read_process_start_ticks(_pid: u32) -> Result<u64> {
    bail!("process identity is unsupported on this platform")
}

#[cfg(all(test, target_os = "linux"))]
#[path = "process_tests.rs"]
mod tests;
