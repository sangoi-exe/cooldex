use std::process::Stdio;

use super::ProcessIdentity;
use super::ProcessSignal;
#[cfg(target_os = "linux")]
use super::arm_parent_death_sigkill;
#[cfg(target_os = "linux")]
use super::process_exists;
use super::send_signal;

#[tokio::test]
async fn captured_identity_matches_only_the_same_process_start() {
    let identity = ProcessIdentity::current()
        .await
        .expect("capture current process");
    let different_start = ProcessIdentity::from_parts(
        identity.pid(),
        format!("{}-different", identity.process_start_time()),
    )
    .expect("different identity");

    assert!(identity.is_active().await.expect("match current process"));
    assert!(!different_start.is_active().await.expect("reject mismatch"));
}

#[tokio::test]
async fn signaled_child_becomes_inactive_after_reap() {
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child");
    let pid = child.id().expect("child pid");
    let identity = ProcessIdentity::capture(pid).await.expect("capture child");

    send_signal(pid, ProcessSignal::Terminate).expect("terminate child");
    let status = child.wait().await.expect("reap child");

    assert!(!status.success());
    assert!(!identity.is_active().await.expect("check child"));
}

#[cfg(target_os = "linux")]
fn create_pipe() -> [libc::c_int; 2] {
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    descriptors
}

#[cfg(target_os = "linux")]
fn write_fd(descriptor: libc::c_int, bytes: &[u8]) {
    let mut written = 0;
    while written < bytes.len() {
        let result = unsafe {
            libc::write(
                descriptor,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        assert!(
            result > 0,
            "pipe write failed: {}",
            std::io::Error::last_os_error()
        );
        written += result.unsigned_abs();
    }
}

#[cfg(target_os = "linux")]
fn read_fd(descriptor: libc::c_int, bytes: &mut [u8]) {
    let mut read = 0;
    while read < bytes.len() {
        let result = unsafe {
            libc::read(
                descriptor,
                bytes[read..].as_mut_ptr().cast(),
                bytes.len() - read,
            )
        };
        assert!(
            result > 0,
            "pipe read failed: {}",
            std::io::Error::last_os_error()
        );
        read += result.unsigned_abs();
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn parent_death_signal_kills_an_armed_child() {
    let child_pid_pipe = create_pipe();
    let intermediate_pid = unsafe { libc::fork() };
    assert!(
        intermediate_pid >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );

    if intermediate_pid == 0 {
        unsafe { libc::close(child_pid_pipe[0]) };
        let armed_pipe = create_pipe();
        let protected_pid = unsafe { libc::fork() };
        if protected_pid == 0 {
            unsafe {
                libc::close(child_pid_pipe[1]);
                libc::close(armed_pipe[0]);
            }
            let parent_pid = unsafe { libc::getppid() };
            if arm_parent_death_sigkill(parent_pid).is_err() {
                unsafe { libc::_exit(2) };
            }
            write_fd(armed_pipe[1], &[1]);
            unsafe { libc::close(armed_pipe[1]) };
            loop {
                unsafe { libc::pause() };
            }
        }
        if protected_pid < 0 {
            unsafe { libc::_exit(3) };
        }
        unsafe { libc::close(armed_pipe[1]) };
        let mut armed = [0];
        read_fd(armed_pipe[0], &mut armed);
        unsafe { libc::close(armed_pipe[0]) };
        write_fd(child_pid_pipe[1], &protected_pid.to_ne_bytes());
        unsafe {
            libc::close(child_pid_pipe[1]);
            libc::_exit(0);
        }
    }

    unsafe { libc::close(child_pid_pipe[1]) };
    let mut child_pid_bytes = [0; std::mem::size_of::<libc::pid_t>()];
    read_fd(child_pid_pipe[0], &mut child_pid_bytes);
    unsafe { libc::close(child_pid_pipe[0]) };
    let protected_pid = libc::pid_t::from_ne_bytes(child_pid_bytes);
    let mut intermediate_status = 0;
    assert_eq!(
        unsafe { libc::waitpid(intermediate_pid, &mut intermediate_status, 0) },
        intermediate_pid
    );
    assert!(libc::WIFEXITED(intermediate_status));
    assert_eq!(libc::WEXITSTATUS(intermediate_status), 0);

    let protected_pid = u32::try_from(protected_pid).expect("positive protected child PID");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_exists(protected_pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        !process_exists(protected_pid),
        "protected child {protected_pid} survived parent death"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn parent_race_check_rejects_a_changed_parent() {
    let child_pid = unsafe { libc::fork() };
    assert!(
        child_pid >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    if child_pid == 0 {
        let wrong_parent = unsafe { libc::getppid() }.saturating_add(1);
        let rejected = arm_parent_death_sigkill(wrong_parent)
            .is_err_and(|err| err.kind() == std::io::ErrorKind::BrokenPipe);
        unsafe { libc::_exit(if rejected { 0 } else { 1 }) };
    }

    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(child_pid, &mut status, 0) },
        child_pid
    );
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}
