// Merge-safety anchor: apply_patch runtime tests are followers of the explicit host-local vs
// executor-fs runtime split; merges must keep absolute-path/error ownership aligned with
// `runtimes/apply_patch.rs` and the library owners.

use super::*;
use async_trait::async_trait;
use codex_apply_patch::MaybeApplyPatchVerified;
use codex_exec_server::CopyOptions;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileMetadata;
use codex_exec_server::LOCAL_FS;
use codex_exec_server::ReadDirectoryEntry;
use codex_exec_server::RemoveOptions;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::PathBufExt;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(not(target_os = "windows"))]
use std::path::Path;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::tempdir;

#[test]
fn wants_no_sandbox_approval_granular_respects_sandbox_flag() {
    let runtime = ApplyPatchRuntime::new();
    assert!(runtime.wants_no_sandbox_approval(AskForApproval::OnRequest));
    assert!(
        !runtime.wants_no_sandbox_approval(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: false,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
    assert!(
        runtime.wants_no_sandbox_approval(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
}

#[test]
fn guardian_review_request_includes_patch_context() {
    let path = std::env::temp_dir()
        .join("guardian-apply-patch-test.txt")
        .abs();
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let expected_cwd = action.cwd.to_path_buf();
    let expected_patch = action.patch.clone();
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![path.clone()],
        changes: HashMap::from([(
            path.to_path_buf(),
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };

    let guardian_request = ApplyPatchRuntime::build_guardian_review_request(&request, "call-1");

    assert_eq!(
        guardian_request,
        GuardianApprovalRequest::ApplyPatch {
            id: "call-1".to_string(),
            cwd: expected_cwd,
            files: request.file_paths,
            patch: expected_patch,
        }
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn build_sandbox_command_prefers_configured_codex_self_exe_for_apply_patch() {
    let path = std::env::temp_dir()
        .join("apply-patch-current-exe-test.txt")
        .abs();
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![path.clone()],
        changes: HashMap::from([(
            path.to_path_buf(),
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };
    let codex_self_exe = PathBuf::from("/tmp/codex");

    let command = ApplyPatchRuntime::build_sandbox_command(&request, Some(&codex_self_exe))
        .expect("build sandbox command");

    assert_eq!(command.program, codex_self_exe.into_os_string());
}

#[cfg(not(target_os = "windows"))]
#[test]
fn build_sandbox_command_falls_back_to_current_exe_for_apply_patch() {
    let path = std::env::temp_dir()
        .join("apply-patch-current-exe-test.txt")
        .abs();
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![path.clone()],
        changes: HashMap::from([(
            path.to_path_buf(),
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };

    let command = ApplyPatchRuntime::build_sandbox_command(&request, /*codex_self_exe*/ None)
        .expect("build sandbox command");

    assert_eq!(
        command.program,
        std::env::current_exe()
            .expect("current exe")
            .into_os_string()
    );
}

#[derive(Default)]
struct RecordingExecutorFileSystem {
    directories: Mutex<HashSet<AbsolutePathBuf>>,
    files: Mutex<HashMap<AbsolutePathBuf, Vec<u8>>>,
}

impl RecordingExecutorFileSystem {
    fn file_contents(&self, path: &AbsolutePathBuf) -> Option<Vec<u8>> {
        self.files.lock().expect("lock files").get(path).cloned()
    }

    fn not_found(path: &AbsolutePathBuf) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} not found", path.display()),
        )
    }
}

#[async_trait]
impl ExecutorFileSystem for RecordingExecutorFileSystem {
    async fn read_file(&self, path: &AbsolutePathBuf) -> std::io::Result<Vec<u8>> {
        self.file_contents(path)
            .ok_or_else(|| Self::not_found(path))
    }

    async fn write_file(&self, path: &AbsolutePathBuf, contents: Vec<u8>) -> std::io::Result<()> {
        self.files
            .lock()
            .expect("lock files")
            .insert(path.clone(), contents);
        Ok(())
    }

    async fn create_directory(
        &self,
        path: &AbsolutePathBuf,
        options: CreateDirectoryOptions,
    ) -> std::io::Result<()> {
        let mut directories = self.directories.lock().expect("lock directories");
        directories.insert(path.clone());
        if options.recursive {
            let mut cursor = path.parent();
            while let Some(parent) = cursor {
                directories.insert(parent.clone());
                cursor = parent.parent();
            }
        }
        Ok(())
    }

    async fn get_metadata(&self, path: &AbsolutePathBuf) -> std::io::Result<FileMetadata> {
        if self.files.lock().expect("lock files").contains_key(path) {
            return Ok(FileMetadata {
                is_directory: false,
                is_file: true,
                created_at_ms: 0,
                modified_at_ms: 0,
            });
        }
        if self
            .directories
            .lock()
            .expect("lock directories")
            .contains(path)
        {
            return Ok(FileMetadata {
                is_directory: true,
                is_file: false,
                created_at_ms: 0,
                modified_at_ms: 0,
            });
        }
        Err(Self::not_found(path))
    }

    async fn read_directory(
        &self,
        _path: &AbsolutePathBuf,
    ) -> std::io::Result<Vec<ReadDirectoryEntry>> {
        Ok(Vec::new())
    }

    async fn remove(&self, path: &AbsolutePathBuf, _options: RemoveOptions) -> std::io::Result<()> {
        if self
            .files
            .lock()
            .expect("lock files")
            .remove(path)
            .is_some()
        {
            return Ok(());
        }
        if self
            .directories
            .lock()
            .expect("lock directories")
            .remove(path)
        {
            return Ok(());
        }
        Err(Self::not_found(path))
    }

    async fn copy(
        &self,
        source_path: &AbsolutePathBuf,
        destination_path: &AbsolutePathBuf,
        _options: CopyOptions,
    ) -> std::io::Result<()> {
        let Some(contents) = self.file_contents(source_path) else {
            return Err(Self::not_found(source_path));
        };
        self.files
            .lock()
            .expect("lock files")
            .insert(destination_path.clone(), contents);
        Ok(())
    }
}

#[tokio::test]
async fn remote_filesystem_path_mutates_only_executor_fs() {
    let dir = tempdir().expect("tempdir");
    let cwd = dir.path().abs();
    let relative_path = Path::new("nested/remote.txt");
    let target = AbsolutePathBuf::resolve_path_against_base(relative_path, &cwd);
    std::fs::create_dir_all(dir.path().join("nested")).expect("create local nested dir");
    std::fs::write(dir.path().join("nested/remote.txt"), "host-local\n").expect("seed local file");

    let patch = "*** Begin Patch\n*** Add File: nested/remote.txt\n+remote-executor\n*** End Patch";
    let command = vec!["apply_patch".to_string(), patch.to_string()];
    let action = match codex_apply_patch::maybe_parse_apply_patch_verified(
        &command,
        &cwd,
        LOCAL_FS.as_ref(),
    )
    .await
    {
        MaybeApplyPatchVerified::Body(action) => action,
        other => panic!("expected parsed apply_patch action, got {other:?}"),
    };
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![target.clone()],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: true,
        timeout_ms: None,
    };
    let fs = RecordingExecutorFileSystem::default();

    let output = ApplyPatchRuntime::run_with_executor_filesystem(&request, &fs).await;

    assert_eq!(output.exit_code, 0);
    assert_eq!(
        output.stdout.text,
        "Success. Updated the following files:\nA nested/remote.txt\n"
    );
    assert_eq!(output.stderr.text, "");
    assert_eq!(
        fs.file_contents(&target).expect("remote file contents"),
        b"remote-executor\n".to_vec()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("nested/remote.txt")).expect("read local file"),
        "host-local\n"
    );
}

#[tokio::test]
async fn remote_filesystem_move_reports_destination_path() {
    let dir = tempdir().expect("tempdir");
    let cwd = dir.path().abs();
    let source_rel = Path::new("old/name.txt");
    let source_abs = AbsolutePathBuf::resolve_path_against_base(source_rel, &cwd);
    let dest_abs =
        AbsolutePathBuf::resolve_path_against_base(Path::new("renamed/dir/name.txt"), &cwd);
    std::fs::create_dir_all(dir.path().join("old")).expect("create local old dir");
    std::fs::write(dir.path().join("old/name.txt"), "host-local\n").expect("seed local file");

    let patch = "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n-host-local\n+remote-executor\n*** End Patch";
    let command = vec!["apply_patch".to_string(), patch.to_string()];
    let action = match codex_apply_patch::maybe_parse_apply_patch_verified(
        &command,
        &cwd,
        LOCAL_FS.as_ref(),
    )
    .await
    {
        MaybeApplyPatchVerified::Body(action) => action,
        other => panic!("expected parsed apply_patch action, got {other:?}"),
    };
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![source_abs.clone(), dest_abs.clone()],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: true,
        timeout_ms: None,
    };
    let fs = RecordingExecutorFileSystem::default();
    fs.files
        .lock()
        .expect("lock files")
        .insert(source_abs.clone(), b"host-local\n".to_vec());

    let output = ApplyPatchRuntime::run_with_executor_filesystem(&request, &fs).await;

    assert_eq!(output.exit_code, 0);
    assert_eq!(
        output.stdout.text,
        "Success. Updated the following files:\nM renamed/dir/name.txt\n"
    );
    assert_eq!(output.stderr.text, "");
    assert!(fs.file_contents(&source_abs).is_none());
    assert_eq!(
        fs.file_contents(&dest_abs).expect("remote dest contents"),
        b"remote-executor\n".to_vec()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("old/name.txt")).expect("read local source"),
        "host-local\n"
    );
    assert!(!dir.path().join("renamed/dir/name.txt").exists());
}
