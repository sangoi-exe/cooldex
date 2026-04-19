mod invocation;
mod parser;
mod seek_sequence;
mod standalone_executable;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::RemoveOptions;
use codex_utils_absolute_path::AbsolutePathBuf;
pub use parser::Hunk;
pub use parser::ParseError;
use parser::ParseError::*;
pub use parser::UpdateFileChunk;
pub use parser::parse_patch;
pub use parser::parse_patch_streaming;
use similar::TextDiff;
use thiserror::Error;

pub use invocation::maybe_parse_apply_patch_verified;
pub use standalone_executable::main;

use crate::invocation::ExtractHeredocError;

// Merge-safety anchor: local apply-patch keeps the host-local atomic rollback owner separate
// from the executor-fs-backed remote owner; merges must not collapse those paths back into a
// single filesystem implementation.

/// Detailed instructions for gpt-4.1 on how to use the `apply_patch` tool.
pub const APPLY_PATCH_TOOL_INSTRUCTIONS: &str = include_str!("../apply_patch_tool_instructions.md");

/// Special argv[1] flag used when the Codex executable self-invokes to run the
/// internal `apply_patch` path.
///
/// Although this constant lives in `codex-apply-patch` (to avoid forcing
/// `codex-arg0` to depend on `codex-core`), it remains part of the "codex core"
/// process-invocation contract for the standalone `apply_patch` command
/// surface.
pub const CODEX_CORE_APPLY_PATCH_ARG1: &str = "--codex-run-as-apply-patch";

#[derive(Debug, Error, PartialEq)]
pub enum ApplyPatchError {
    #[error(transparent)]
    ParseError(#[from] ParseError),
    #[error(transparent)]
    IoError(#[from] IoError),
    /// Error that occurs while computing replacements when applying patch chunks
    #[error("{0}")]
    ComputeReplacements(String),
    /// A raw patch body was provided without an explicit `apply_patch` invocation.
    #[error(
        "patch detected without explicit call to apply_patch. Rerun as [\"apply_patch\", \"<patch>\"]"
    )]
    ImplicitInvocation,
}

impl From<std::io::Error> for ApplyPatchError {
    fn from(err: std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: err,
        })
    }
}

impl From<&std::io::Error> for ApplyPatchError {
    fn from(err: &std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: std::io::Error::new(err.kind(), err.to_string()),
        })
    }
}

#[derive(Debug, Error)]
#[error("{context}: {source}")]
pub struct IoError {
    context: String,
    #[source]
    source: std::io::Error,
}

impl PartialEq for IoError {
    fn eq(&self, other: &Self) -> bool {
        self.context == other.context && self.source.to_string() == other.source.to_string()
    }
}

/// Both the raw PATCH argument to `apply_patch` as well as the PATCH argument
/// parsed into hunks.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchArgs {
    pub patch: String,
    pub hunks: Vec<Hunk>,
    pub workdir: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum ApplyPatchFileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
        /// new_content that will result after the unified_diff is applied.
        new_content: String,
    },
}

#[derive(Debug, PartialEq)]
pub enum MaybeApplyPatchVerified {
    /// `argv` corresponded to an `apply_patch` invocation, and these are the
    /// resulting proposed file changes.
    Body(ApplyPatchAction),
    /// `argv` could not be parsed to determine whether it corresponds to an
    /// `apply_patch` invocation.
    ShellParseError(ExtractHeredocError),
    /// `argv` corresponded to an `apply_patch` invocation, but it could not
    /// be fulfilled due to the specified error.
    CorrectnessError(ApplyPatchError),
    /// `argv` decidedly did not correspond to an `apply_patch` invocation.
    NotApplyPatch,
}

/// ApplyPatchAction is the result of parsing an `apply_patch` command. By
/// construction, all paths should be absolute paths.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchAction {
    changes: HashMap<PathBuf, ApplyPatchFileChange>,

    /// The raw patch argument that can be used to apply the patch. i.e., if the
    /// original arg was parsed in "lenient" mode with a
    /// heredoc, this should be the value without the heredoc wrapper.
    pub patch: String,

    /// The working directory that was used to resolve relative paths in the patch.
    pub cwd: AbsolutePathBuf,
}

impl ApplyPatchAction {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Returns the changes that would be made by applying the patch.
    pub fn changes(&self) -> &HashMap<PathBuf, ApplyPatchFileChange> {
        &self.changes
    }

    /// Should be used exclusively for testing. (Not worth the overhead of
    /// creating a feature flag for this.)
    pub fn new_add_for_test(path: &AbsolutePathBuf, content: String) -> Self {
        #[expect(clippy::expect_used)]
        let filename = path
            .file_name()
            .expect("path should not be empty")
            .to_string_lossy();
        let patch = format!(
            r#"*** Begin Patch
*** Update File: {filename}
@@
+ {content}
*** End Patch"#,
        );
        let changes = HashMap::from([(path.to_path_buf(), ApplyPatchFileChange::Add { content })]);
        #[expect(clippy::expect_used)]
        Self {
            changes,
            cwd: path.parent().expect("path should have parent"),
            patch,
        }
    }
}

/// Applies the patch against the host-local filesystem and prints the result to stdout/stderr.
pub async fn apply_patch(
    patch: &str,
    cwd: &AbsolutePathBuf,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<(), ApplyPatchError> {
    let hunks = parse_patch_for_apply(patch, stderr)?;

    apply_hunks(&hunks, cwd, stdout, stderr).await?;

    Ok(())
}

/// Applies the patch through the provided executor filesystem.
pub async fn apply_patch_via_executor_fs(
    patch: &str,
    cwd: &AbsolutePathBuf,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ApplyPatchError> {
    let hunks = parse_patch_for_apply(patch, stderr)?;

    apply_hunks_via_executor_fs(&hunks, cwd, stdout, stderr, fs, sandbox).await?;

    Ok(())
}

/// Applies hunks and continues to update stdout/stderr
pub async fn apply_hunks(
    hunks: &[Hunk],
    cwd: &AbsolutePathBuf,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<(), ApplyPatchError> {
    // Delegate to a helper that applies each hunk to the filesystem.
    match apply_hunks_to_files(hunks, cwd).await {
        Ok(affected) => {
            print_summary(&affected, stdout).map_err(ApplyPatchError::from)?;
            Ok(())
        }
        Err(err) => {
            let msg = err.to_string();
            writeln!(stderr, "{msg}").map_err(ApplyPatchError::from)?;
            if let Some(io) = err.downcast_ref::<std::io::Error>() {
                Err(ApplyPatchError::from(io))
            } else {
                Err(ApplyPatchError::IoError(IoError {
                    context: msg,
                    source: std::io::Error::other(err),
                }))
            }
        }
    }
}

/// Applies hunks through the provided executor filesystem and continues to update stdout/stderr.
pub async fn apply_hunks_via_executor_fs(
    hunks: &[Hunk],
    cwd: &AbsolutePathBuf,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ApplyPatchError> {
    match apply_hunks_to_executor_fs(hunks, cwd, fs, sandbox).await {
        Ok(affected) => {
            print_summary(&affected, stdout).map_err(ApplyPatchError::from)?;
            Ok(())
        }
        Err(err) => {
            let msg = err.to_string();
            writeln!(stderr, "{msg}").map_err(ApplyPatchError::from)?;
            if let Some(io_error) = err.downcast_ref::<std::io::Error>() {
                Err(ApplyPatchError::from(io_error))
            } else {
                Err(ApplyPatchError::IoError(IoError {
                    context: msg,
                    source: std::io::Error::other(err),
                }))
            }
        }
    }
}

/// Applies each parsed patch hunk to the filesystem.
/// Returns an error if any of the changes could not be applied.
/// Tracks file paths affected by applying a patch, preserving the path spelling
/// from the patch for user-facing summaries.
pub struct AffectedPaths {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
enum ExistingFileSnapshot {
    RegularFile {
        contents: Vec<u8>,
        permissions: std::fs::Permissions,
    },
    Symlink {
        target: PathBuf,
        target_contents: Vec<u8>,
        target_permissions: std::fs::Permissions,
    },
    SymlinkPath {
        target: PathBuf,
    },
}

impl ExistingFileSnapshot {
    fn current_contents(&self) -> &[u8] {
        match self {
            ExistingFileSnapshot::RegularFile { contents, .. } => contents,
            ExistingFileSnapshot::Symlink {
                target_contents, ..
            } => target_contents,
            ExistingFileSnapshot::SymlinkPath { .. } => {
                unreachable!("SymlinkPath snapshots are only for delete/move-source rollback")
            }
        }
    }

    fn current_permissions(&self) -> &std::fs::Permissions {
        match self {
            ExistingFileSnapshot::RegularFile { permissions, .. } => permissions,
            ExistingFileSnapshot::Symlink {
                target_permissions, ..
            } => target_permissions,
            ExistingFileSnapshot::SymlinkPath { .. } => {
                unreachable!("SymlinkPath snapshots are only for delete/move-source rollback")
            }
        }
    }
}

#[derive(Debug, Clone)]
enum VirtualEntry {
    Missing,
    RegularFile { contents: String },
    Symlink { target: PathBuf, contents: String },
}

impl VirtualEntry {
    fn current_contents(&self) -> Option<&str> {
        match self {
            VirtualEntry::Missing => None,
            VirtualEntry::RegularFile { contents } => Some(contents),
            VirtualEntry::Symlink { contents, .. } => Some(contents),
        }
    }

    fn with_updated_contents(&self, contents: String) -> Self {
        match self {
            VirtualEntry::Missing | VirtualEntry::RegularFile { .. } => {
                VirtualEntry::RegularFile { contents }
            }
            VirtualEntry::Symlink { target, .. } => VirtualEntry::Symlink {
                target: target.clone(),
                contents,
            },
        }
    }
}

#[derive(Debug)]
enum PreparedChange {
    Add {
        display_path: PathBuf,
        path_abs: AbsolutePathBuf,
        contents: String,
    },
    Delete {
        display_path: PathBuf,
        path_abs: AbsolutePathBuf,
    },
    Update {
        display_path: PathBuf,
        path_abs: AbsolutePathBuf,
        contents: String,
    },
    Move {
        source_path_abs: AbsolutePathBuf,
        dest_display_path: PathBuf,
        dest_path_abs: AbsolutePathBuf,
        contents: String,
    },
}

#[derive(Debug)]
enum RollbackChange {
    RemoveFile {
        path: PathBuf,
        created_dirs: Vec<PathBuf>,
    },
    RestoreExistingPath {
        path: PathBuf,
        snapshot: ExistingFileSnapshot,
    },
    RestoreDeletedPath {
        path: PathBuf,
        snapshot: ExistingFileSnapshot,
    },
}

/// Apply the hunks to the filesystem, returning which files were added, modified, or deleted.
/// Returns an error if the patch could not be applied.
async fn apply_hunks_to_files(
    hunks: &[Hunk],
    cwd: &AbsolutePathBuf,
) -> anyhow::Result<AffectedPaths> {
    if hunks.is_empty() {
        anyhow::bail!("No files were modified.");
    }

    let prepared_changes = prepare_changes(hunks, cwd)?;
    let mut added: Vec<PathBuf> = Vec::new();
    let mut modified: Vec<PathBuf> = Vec::new();
    let mut deleted: Vec<PathBuf> = Vec::new();
    let mut rollbacks: Vec<RollbackChange> = Vec::new();

    for prepared_change in prepared_changes {
        if let Err(err) = commit_prepared_change(
            prepared_change,
            &mut added,
            &mut modified,
            &mut deleted,
            &mut rollbacks,
        ) {
            let rollback_result = rollback_changes(rollbacks);
            return match rollback_result {
                Ok(()) => Err(err),
                Err(rollback_err) => Err(err.context(format!("Rollback failed: {rollback_err}"))),
            };
        }
    }

    Ok(AffectedPaths {
        added,
        modified,
        deleted,
    })
}

async fn apply_hunks_to_executor_fs(
    hunks: &[Hunk],
    cwd: &AbsolutePathBuf,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> anyhow::Result<AffectedPaths> {
    if hunks.is_empty() {
        anyhow::bail!("No files were modified.");
    }

    let mut added: Vec<PathBuf> = Vec::new();
    let mut modified: Vec<PathBuf> = Vec::new();
    let mut deleted: Vec<PathBuf> = Vec::new();
    for hunk in hunks {
        let affected_path = hunk.path().to_path_buf();
        let path_abs = hunk.resolve_path(cwd);
        match hunk {
            Hunk::AddFile { contents, .. } => {
                write_file_with_missing_parent_retry(
                    fs,
                    &path_abs,
                    contents.clone().into_bytes(),
                    sandbox,
                )
                .await?;
                added.push(affected_path);
            }
            Hunk::DeleteFile { .. } => {
                let result: io::Result<()> = async {
                    let metadata = fs.get_metadata(&path_abs, sandbox).await?;
                    if metadata.is_directory {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "path is a directory",
                        ));
                    }
                    fs.remove(
                        &path_abs,
                        RemoveOptions {
                            recursive: false,
                            force: false,
                        },
                        sandbox,
                    )
                    .await
                }
                .await;
                result.with_context(|| format!("Failed to delete file {}", path_abs.display()))?;
                deleted.push(affected_path);
            }
            Hunk::UpdateFile {
                move_path, chunks, ..
            } => {
                let AppliedPatch { new_contents, .. } =
                    derive_new_contents_from_chunks(&path_abs, chunks, fs, sandbox).await?;
                if let Some(dest) = move_path {
                    let dest_display_path = dest.to_path_buf();
                    let dest_abs = AbsolutePathBuf::resolve_path_against_base(dest, cwd);
                    write_file_with_missing_parent_retry(
                        fs,
                        &dest_abs,
                        new_contents.into_bytes(),
                        sandbox,
                    )
                    .await?;
                    let result: io::Result<()> = async {
                        let metadata = fs.get_metadata(&path_abs, sandbox).await?;
                        if metadata.is_directory {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "path is a directory",
                            ));
                        }
                        fs.remove(
                            &path_abs,
                            RemoveOptions {
                                recursive: false,
                                force: false,
                            },
                            sandbox,
                        )
                        .await
                    }
                    .await;
                    result.with_context(|| {
                        format!("Failed to remove original {}", path_abs.display())
                    })?;
                    modified.push(dest_display_path);
                } else {
                    fs.write_file(&path_abs, new_contents.into_bytes(), sandbox)
                        .await
                        .with_context(|| format!("Failed to write file {}", path_abs.display()))?;
                    modified.push(affected_path);
                }
            }
        }
    }

    Ok(AffectedPaths {
        added,
        modified,
        deleted,
    })
}

fn prepare_changes(hunks: &[Hunk], cwd: &AbsolutePathBuf) -> anyhow::Result<Vec<PreparedChange>> {
    let mut prepared_changes = Vec::with_capacity(hunks.len());
    let mut virtual_entries: HashMap<PathBuf, VirtualEntry> = HashMap::new();

    for hunk in hunks {
        let path_abs = hunk.resolve_path(cwd);
        match hunk {
            Hunk::AddFile { path, contents } => {
                let current_entry = current_virtual_entry(
                    &mut virtual_entries,
                    path_abs.as_path(),
                    load_virtual_entry_for_write,
                )?;
                prepared_changes.push(PreparedChange::Add {
                    display_path: path.clone(),
                    path_abs: path_abs.clone(),
                    contents: contents.clone(),
                });
                virtual_entries.insert(
                    path_abs.to_path_buf(),
                    current_entry.with_updated_contents(contents.clone()),
                );
            }
            Hunk::DeleteFile { path } => {
                let current_entry = current_virtual_entry(
                    &mut virtual_entries,
                    path_abs.as_path(),
                    load_virtual_entry_for_delete,
                )?;
                if matches!(current_entry, VirtualEntry::Missing) {
                    anyhow::bail!("Failed to delete file {}", path_abs.display());
                }
                prepared_changes.push(PreparedChange::Delete {
                    display_path: path.clone(),
                    path_abs: path_abs.clone(),
                });
                virtual_entries.insert(path_abs.to_path_buf(), VirtualEntry::Missing);
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let source_entry = current_virtual_entry(
                    &mut virtual_entries,
                    path_abs.as_path(),
                    load_virtual_entry_for_update,
                )?;
                let Some(source_contents) = source_entry.current_contents() else {
                    return Err(missing_update_error(path_abs.as_path()));
                };
                let AppliedPatch { new_contents, .. } =
                    derive_new_contents_from_contents(path_abs.as_path(), source_contents, chunks)?;
                if let Some(dest_path) = move_path {
                    let dest_path_abs = AbsolutePathBuf::resolve_path_against_base(dest_path, cwd);
                    let dest_entry = current_virtual_entry(
                        &mut virtual_entries,
                        dest_path_abs.as_path(),
                        load_virtual_entry_for_write,
                    )?;
                    prepared_changes.push(PreparedChange::Move {
                        source_path_abs: path_abs.clone(),
                        dest_display_path: dest_path.clone(),
                        dest_path_abs: dest_path_abs.clone(),
                        contents: new_contents.clone(),
                    });
                    virtual_entries.insert(path_abs.to_path_buf(), VirtualEntry::Missing);
                    virtual_entries.insert(
                        dest_path_abs.to_path_buf(),
                        dest_entry.with_updated_contents(new_contents),
                    );
                } else {
                    prepared_changes.push(PreparedChange::Update {
                        display_path: path.clone(),
                        path_abs: path_abs.clone(),
                        contents: new_contents.clone(),
                    });
                    virtual_entries.insert(
                        path_abs.to_path_buf(),
                        source_entry.with_updated_contents(new_contents),
                    );
                }
            }
        }
    }

    Ok(prepared_changes)
}

fn current_virtual_entry(
    virtual_entries: &mut HashMap<PathBuf, VirtualEntry>,
    path: &Path,
    loader: fn(&Path) -> anyhow::Result<VirtualEntry>,
) -> anyhow::Result<VirtualEntry> {
    if let Some(entry) = virtual_entries.get(path) {
        return Ok(entry.clone());
    }

    let entry = loader(path)?;
    virtual_entries.insert(path.to_path_buf(), entry.clone());
    Ok(entry)
}

fn load_virtual_entry_for_write(path: &Path) -> anyhow::Result<VirtualEntry> {
    let failure_context = format!("Failed to write file {}", path.display());
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            OpenOptions::new()
                .write(true)
                .open(path)
                .with_context(|| failure_context.clone())?;
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(path).with_context(|| failure_context.clone())?;
                Ok(VirtualEntry::Symlink {
                    target,
                    contents: String::new(),
                })
            } else if metadata.is_file() {
                Ok(VirtualEntry::RegularFile {
                    contents: String::new(),
                })
            } else {
                anyhow::bail!(failure_context)
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(VirtualEntry::Missing),
        Err(err) => Err(anyhow::Error::new(err).context(failure_context)),
    }
}

fn load_virtual_entry_for_delete(path: &Path) -> anyhow::Result<VirtualEntry> {
    let failure_context = format!("Failed to delete file {}", path.display());
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(path).with_context(|| failure_context.clone())?;
                Ok(VirtualEntry::Symlink {
                    target,
                    contents: String::new(),
                })
            } else if metadata.is_file() {
                Ok(VirtualEntry::RegularFile {
                    contents: String::new(),
                })
            } else {
                anyhow::bail!(failure_context)
            }
        }
        Err(err) => Err(anyhow::Error::new(err).context(failure_context)),
    }
}

fn load_virtual_entry_for_update(path: &Path) -> anyhow::Result<VirtualEntry> {
    let failure_context = format!("Failed to read file to update {}", path.display());
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => return Err(anyhow::anyhow!("{failure_context}: {err}")),
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) => return Err(anyhow::anyhow!("{failure_context}: {err}")),
    };
    if metadata.file_type().is_symlink() {
        let target = match std::fs::read_link(path) {
            Ok(target) => target,
            Err(err) => return Err(anyhow::anyhow!("{failure_context}: {err}")),
        };
        Ok(VirtualEntry::Symlink { target, contents })
    } else if metadata.is_file() {
        Ok(VirtualEntry::RegularFile { contents })
    } else {
        anyhow::bail!(failure_context)
    }
}

fn missing_update_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "Failed to read file to update {}: No such file or directory (os error 2)",
        path.display()
    )
}

fn snapshot_deleted_path(
    path: &Path,
    failure_context: &str,
) -> anyhow::Result<Option<ExistingFileSnapshot>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                let target =
                    std::fs::read_link(path).with_context(|| failure_context.to_string())?;
                Ok(Some(ExistingFileSnapshot::SymlinkPath { target }))
            } else if metadata.is_file() {
                reject_multiply_linked_regular_file(path, &metadata, failure_context)?;
                let contents = std::fs::read(path).with_context(|| failure_context.to_string())?;
                Ok(Some(ExistingFileSnapshot::RegularFile {
                    contents,
                    permissions: metadata.permissions(),
                }))
            } else {
                anyhow::bail!(failure_context.to_string())
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::new(err).context(failure_context.to_string())),
    }
}

fn reject_multiply_linked_regular_file(
    path: &Path,
    metadata: &std::fs::Metadata,
    failure_context: &str,
) -> anyhow::Result<()> {
    if metadata.is_file() && hard_link_count(metadata) > 1 {
        anyhow::bail!(
            "{failure_context}: multiply-linked regular files are not supported for delete or move-source rollback ({})",
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn hard_link_count(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;

    metadata.nlink()
}

#[cfg(windows)]
fn hard_link_count(_metadata: &std::fs::Metadata) -> u64 {
    // Stable std does not expose a hard-link count on Windows for this toolchain.
    1
}

#[cfg(not(any(unix, windows)))]
fn hard_link_count(_metadata: &std::fs::Metadata) -> u64 {
    1
}

fn snapshot_existing_path(
    path: &Path,
    failure_context: &str,
    require_write_access: bool,
) -> anyhow::Result<Option<ExistingFileSnapshot>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if require_write_access {
                OpenOptions::new()
                    .write(true)
                    .open(path)
                    .with_context(|| failure_context.to_string())?;
            }
            if metadata.file_type().is_symlink() {
                let target =
                    std::fs::read_link(path).with_context(|| failure_context.to_string())?;
                let target_contents =
                    std::fs::read(path).with_context(|| failure_context.to_string())?;
                let target_permissions = std::fs::metadata(path)
                    .with_context(|| failure_context.to_string())?
                    .permissions();
                Ok(Some(ExistingFileSnapshot::Symlink {
                    target,
                    target_contents,
                    target_permissions,
                }))
            } else if metadata.is_file() {
                let contents = std::fs::read(path).with_context(|| failure_context.to_string())?;
                Ok(Some(ExistingFileSnapshot::RegularFile {
                    contents,
                    permissions: metadata.permissions(),
                }))
            } else {
                anyhow::bail!(failure_context.to_string())
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::new(err).context(failure_context.to_string())),
    }
}

fn commit_prepared_change(
    prepared_change: PreparedChange,
    added: &mut Vec<PathBuf>,
    modified: &mut Vec<PathBuf>,
    deleted: &mut Vec<PathBuf>,
    rollbacks: &mut Vec<RollbackChange>,
) -> anyhow::Result<()> {
    match prepared_change {
        PreparedChange::Add {
            display_path,
            path_abs,
            contents,
        } => {
            let rollback = commit_write_change(path_abs.as_path(), &contents)?;
            added.push(display_path);
            rollbacks.push(rollback);
        }
        PreparedChange::Delete {
            display_path,
            path_abs,
        } => {
            let rollback = commit_delete_change(path_abs.as_path())?;
            deleted.push(display_path);
            rollbacks.push(rollback);
        }
        PreparedChange::Update {
            display_path,
            path_abs,
            contents,
        } => {
            let rollback = commit_write_change(path_abs.as_path(), &contents)?;
            modified.push(display_path);
            rollbacks.push(rollback);
        }
        PreparedChange::Move {
            source_path_abs,
            dest_display_path,
            dest_path_abs,
            contents,
        } => {
            let source_failure_context =
                format!("Failed to remove original {}", source_path_abs.display());
            let source_snapshot =
                snapshot_deleted_path(source_path_abs.as_path(), &source_failure_context)?
                    .ok_or_else(|| {
                        anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
                            .context(source_failure_context.clone())
                    })?;
            let dest_rollback = commit_write_change(dest_path_abs.as_path(), &contents)?;
            if let Err(err) = std::fs::remove_file(source_path_abs.as_path())
                .with_context(|| source_failure_context.clone())
            {
                return match rollback_changes(vec![dest_rollback]) {
                    Ok(()) => Err(err),
                    Err(rollback_err) => {
                        Err(err.context(format!("Rollback failed: {rollback_err}")))
                    }
                };
            }
            modified.push(dest_display_path);
            rollbacks.push(dest_rollback);
            rollbacks.push(RollbackChange::RestoreDeletedPath {
                path: source_path_abs.to_path_buf(),
                snapshot: source_snapshot,
            });
        }
    }

    Ok(())
}

fn commit_write_change(path: &Path, contents: &str) -> anyhow::Result<RollbackChange> {
    let failure_context = format!("Failed to write file {}", path.display());
    match snapshot_existing_path(path, &failure_context, /*require_write_access*/ true)? {
        Some(snapshot) => {
            write_existing_path_with_rollback(path, contents, &snapshot)
                .with_context(|| failure_context.clone())?;
            Ok(RollbackChange::RestoreExistingPath {
                path: path.to_path_buf(),
                snapshot,
            })
        }
        None => {
            let created_dirs = create_parent_dirs_if_needed(path)?;
            if let Err(err) =
                write_new_path_atomically(path, contents.as_bytes(), /*permissions*/ None)
            {
                let cleanup_result = remove_created_dirs_if_empty(&created_dirs);
                return match cleanup_result {
                    Ok(()) => Err(anyhow::Error::new(err).context(failure_context)),
                    Err(cleanup_err) => Err(anyhow::Error::new(err)
                        .context(format!("{failure_context}; cleanup failed: {cleanup_err}"))),
                };
            }
            Ok(RollbackChange::RemoveFile {
                path: path.to_path_buf(),
                created_dirs,
            })
        }
    }
}

fn commit_delete_change(path: &Path) -> anyhow::Result<RollbackChange> {
    let failure_context = format!("Failed to delete file {}", path.display());
    let snapshot = snapshot_deleted_path(path, &failure_context)?.ok_or_else(|| {
        anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
            .context(failure_context.clone())
    })?;
    std::fs::remove_file(path).with_context(|| failure_context.clone())?;
    Ok(RollbackChange::RestoreDeletedPath {
        path: path.to_path_buf(),
        snapshot,
    })
}

fn write_existing_path_with_rollback(
    path: &Path,
    contents: &str,
    snapshot: &ExistingFileSnapshot,
) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("Failed to write file {}", path.display()))?;
    file.set_len(0)
        .with_context(|| format!("Failed to write file {}", path.display()))?;
    let write_result = (|| -> std::io::Result<()> {
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    match write_result {
        Ok(()) => Ok(()),
        Err(err) => match restore_existing_path(path, snapshot) {
            Ok(()) => Err(anyhow::Error::new(err)),
            Err(rollback_err) => {
                Err(anyhow::Error::new(err).context(format!("Rollback failed: {rollback_err}")))
            }
        },
    }
}

fn restore_existing_path(path: &Path, snapshot: &ExistingFileSnapshot) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("Failed to restore {}", path.display()))?;
    file.set_len(0)
        .with_context(|| format!("Failed to restore {}", path.display()))?;
    file.write_all(snapshot.current_contents())
        .with_context(|| format!("Failed to restore {}", path.display()))?;
    file.flush()
        .with_context(|| format!("Failed to restore {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to restore {}", path.display()))?;
    drop(file);
    std::fs::set_permissions(path, snapshot.current_permissions().clone())
        .with_context(|| format!("Failed to restore {}", path.display()))?;
    Ok(())
}

fn rollback_changes(rollbacks: Vec<RollbackChange>) -> anyhow::Result<()> {
    for rollback in rollbacks.into_iter().rev() {
        match rollback {
            RollbackChange::RemoveFile { path, created_dirs } => {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(anyhow::Error::new(err).context(format!(
                            "Failed to remove rollback file {}",
                            path.display()
                        )));
                    }
                }
                remove_created_dirs_if_empty(&created_dirs)?;
            }
            RollbackChange::RestoreExistingPath { path, snapshot } => {
                restore_existing_path(&path, &snapshot)?;
            }
            RollbackChange::RestoreDeletedPath { path, snapshot } => {
                restore_deleted_path(&path, &snapshot)?;
            }
        }
    }

    Ok(())
}

fn restore_deleted_path(path: &Path, snapshot: &ExistingFileSnapshot) -> anyhow::Result<()> {
    let created_dirs = create_parent_dirs_if_needed(path)?;
    let restore_result = match snapshot {
        ExistingFileSnapshot::RegularFile {
            contents,
            permissions,
        } => write_new_path_atomically(path, contents, Some(permissions)),
        ExistingFileSnapshot::Symlink { target, .. }
        | ExistingFileSnapshot::SymlinkPath { target } => create_symlink(target, path),
    };
    match restore_result {
        Ok(()) => Ok(()),
        Err(err) => {
            let cleanup_result = remove_created_dirs_if_empty(&created_dirs);
            match cleanup_result {
                Ok(()) => Err(anyhow::Error::new(err)
                    .context(format!("Failed to restore {}", path.display()))),
                Err(cleanup_err) => Err(anyhow::Error::new(err).context(format!(
                    "Failed to restore {}: cleanup failed: {cleanup_err}",
                    path.display()
                ))),
            }
        }
    }
}

async fn write_file_with_missing_parent_retry(
    fs: &dyn ExecutorFileSystem,
    path_abs: &AbsolutePathBuf,
    contents: Vec<u8>,
    sandbox: Option<&FileSystemSandboxContext>,
) -> anyhow::Result<()> {
    match fs.write_file(path_abs, contents.clone(), sandbox).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Some(parent_abs) = path_abs.parent() {
                fs.create_directory(
                    &parent_abs,
                    CreateDirectoryOptions { recursive: true },
                    sandbox,
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to create parent directories for {}",
                        path_abs.display()
                    )
                })?;
            }
            fs.write_file(path_abs, contents, sandbox)
                .await
                .with_context(|| format!("Failed to write file {}", path_abs.display()))?;
            Ok(())
        }
        Err(err) => {
            Err(err).with_context(|| format!("Failed to write file {}", path_abs.display()))
        }
    }
}

fn create_parent_dirs_if_needed(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(Vec::new());
    };

    let mut missing_dirs: Vec<PathBuf> = Vec::new();
    let mut current = parent;
    while !current.as_os_str().is_empty() && !current.exists() {
        missing_dirs.push(current.to_path_buf());
        let Some(next) = current.parent().filter(|next| !next.as_os_str().is_empty()) else {
            break;
        };
        if next == current {
            break;
        }
        current = next;
    }

    for dir in missing_dirs.iter().rev() {
        std::fs::create_dir(dir).with_context(|| {
            format!("Failed to create parent directories for {}", path.display())
        })?;
    }

    Ok(missing_dirs)
}

fn remove_created_dirs_if_empty(created_dirs: &[PathBuf]) -> anyhow::Result<()> {
    for dir in created_dirs {
        match std::fs::remove_dir(dir) {
            Ok(()) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!(
                    "Failed to remove rollback directory {}",
                    dir.display()
                )));
            }
        }
    }
    Ok(())
}

fn write_new_path_atomically(
    path: &Path,
    contents: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (mut temp_file, temp_path) = create_atomic_temp_file(parent, path)?;
    let write_result = (|| -> std::io::Result<()> {
        temp_file.write_all(contents)?;
        temp_file.flush()?;
        temp_file.sync_all()?;
        drop(temp_file);
        std::fs::rename(&temp_path, path)?;
        if let Some(permissions) = permissions {
            std::fs::set_permissions(path, permissions.clone())?;
        }
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(windows)]
fn create_symlink(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, path)
}

fn create_atomic_temp_file(
    parent: &Path,
    target_path: &Path,
) -> std::io::Result<(std::fs::File, PathBuf)> {
    let file_stem = target_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "apply-patch".to_string());
    let process_id = std::process::id();
    let timestamp_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..1024_u32 {
        let temp_path = parent.join(format!(
            ".{file_stem}.codex-apply-patch.{process_id}.{timestamp_nanos}.{attempt}.tmp"
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(temp_file) => return Ok((temp_file, temp_path)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("Failed to allocate temp file for {}", target_path.display()),
    ))
}
struct AppliedPatch {
    original_contents: String,
    new_contents: String,
}

fn derive_new_contents_from_contents(
    path: &Path,
    original_contents: &str,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();

    // Drop the trailing empty element that results from the final newline so
    // that line counts match the behaviour of standard `diff`.
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let new_lines = apply_replacements(original_lines, &replacements);
    let mut new_lines = new_lines;
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    let new_contents = new_lines.join("\n");
    Ok(AppliedPatch {
        original_contents: original_contents.to_string(),
        new_contents,
    })
}

/// Return *only* the new file contents (joined into a single `String`) after
/// applying the chunks to the file at `path`.
async fn derive_new_contents_from_chunks(
    path_abs: &AbsolutePathBuf,
    chunks: &[UpdateFileChunk],
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    let original_contents = fs.read_file_text(path_abs, sandbox).await.map_err(|err| {
        ApplyPatchError::IoError(IoError {
            context: format!("Failed to read file to update {}", path_abs.display()),
            source: err,
        })
    })?;

    derive_new_contents_from_contents(path_abs.as_path(), &original_contents, chunks)
}

/// Compute a list of replacements needed to transform `original_lines` into the
/// new lines, given the patch `chunks`. Each replacement is returned as
/// `(start_index, old_len, new_lines)`.
fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<Vec<(usize, usize, Vec<String>)>, ApplyPatchError> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        // If a chunk has a `change_context`, we use seek_sequence to find it, then
        // adjust our `line_index` to continue from there.
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence::seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                /*eof*/ false,
            ) {
                line_index = idx + 1;
            } else {
                return Err(ApplyPatchError::ComputeReplacements(format!(
                    "Failed to find context '{}' in {}",
                    ctx_line,
                    path.display()
                )));
            }
        }

        if chunk.old_lines.is_empty() {
            // Pure addition (no old lines). We'll add them at the end or just
            // before the final empty line if one exists.
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        // Otherwise, try to match the existing lines in the file with the old lines
        // from the chunk. If found, schedule that region for replacement.
        // Attempt to locate the `old_lines` verbatim within the file.  In many
        // real‑world diffs the last element of `old_lines` is an *empty* string
        // representing the terminating newline of the region being replaced.
        // This sentinel is not present in `original_lines` because we strip the
        // trailing empty slice emitted by `split('\n')`.  If a direct search
        // fails and the pattern ends with an empty string, retry without that
        // final element so that modifications touching the end‑of‑file can be
        // located reliably.

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found =
            seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            // Retry without the trailing empty line which represents the final
            // newline in the file.
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }

            found = seek_sequence::seek_sequence(
                original_lines,
                pattern,
                line_index,
                chunk.is_end_of_file,
            );
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            return Err(ApplyPatchError::ComputeReplacements(format!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n"),
            )));
        }
    }

    replacements.sort_by(|(lhs_idx, _, _), (rhs_idx, _, _)| lhs_idx.cmp(rhs_idx));

    Ok(replacements)
}

fn parse_patch_for_apply(
    patch: &str,
    stderr: &mut impl std::io::Write,
) -> Result<Vec<Hunk>, ApplyPatchError> {
    match parse_patch(patch) {
        Ok(source) => Ok(source.hunks),
        Err(e) => {
            match &e {
                InvalidPatchError(message) => {
                    writeln!(stderr, "Invalid patch: {message}").map_err(ApplyPatchError::from)?;
                }
                InvalidHunkError {
                    message,
                    line_number,
                } => {
                    writeln!(
                        stderr,
                        "Invalid patch hunk on line {line_number}: {message}"
                    )
                    .map_err(ApplyPatchError::from)?;
                }
            }
            Err(ApplyPatchError::ParseError(e))
        }
    }
}

/// Apply the `(start_index, old_len, new_lines)` replacements to `original_lines`,
/// returning the modified file contents as a vector of lines.
fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    // We must apply replacements in descending order so that earlier replacements
    // don't shift the positions of later ones.
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let start_idx = *start_idx;
        let old_len = *old_len;

        // Remove old lines.
        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }

        // Insert new lines.
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start_idx + offset, new_line.clone());
        }
    }

    lines
}

/// Intended result of a file update for apply_patch.
#[derive(Debug, Eq, PartialEq)]
pub struct ApplyPatchFileUpdate {
    unified_diff: String,
    content: String,
}

pub async fn unified_diff_from_chunks(
    path_abs: &AbsolutePathBuf,
    chunks: &[UpdateFileChunk],
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> std::result::Result<ApplyPatchFileUpdate, ApplyPatchError> {
    unified_diff_from_chunks_with_context(path_abs, chunks, /*context*/ 1, fs, sandbox).await
}

pub async fn unified_diff_from_chunks_with_context(
    path_abs: &AbsolutePathBuf,
    chunks: &[UpdateFileChunk],
    context: usize,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> std::result::Result<ApplyPatchFileUpdate, ApplyPatchError> {
    let AppliedPatch {
        original_contents,
        new_contents,
    } = derive_new_contents_from_chunks(path_abs, chunks, fs, sandbox).await?;
    let text_diff = TextDiff::from_lines(&original_contents, &new_contents);
    let unified_diff = text_diff.unified_diff().context_radius(context).to_string();
    Ok(ApplyPatchFileUpdate {
        unified_diff,
        content: new_contents,
    })
}

/// Print the summary of changes in git-style format.
/// Write a summary of changes to the given writer.
pub fn print_summary(
    affected: &AffectedPaths,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(out, "Success. Updated the following files:")?;
    for path in &affected.added {
        writeln!(out, "A {}", path.display())?;
    }
    for path in &affected.modified {
        writeln!(out, "M {}", path.display())?;
    }
    for path in &affected.deleted {
        writeln!(out, "D {}", path.display())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_exec_server::LOCAL_FS;
    use codex_utils_absolute_path::test_support::PathExt;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::string::ToString;
    use tempfile::tempdir;

    /// Helper to construct a patch with the given body.
    fn wrap_patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}\n*** End Patch")
    }

    fn apply_patch_for_test(
        patch: &str,
        cwd: &std::path::Path,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> Result<(), ApplyPatchError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(apply_patch(
                patch,
                &AbsolutePathBuf::from_absolute_path(cwd).unwrap(),
                stdout,
                stderr,
            ))
    }

    #[tokio::test]
    async fn test_add_file_hunk_creates_file_with_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("add.txt");
        let patch = wrap_patch(&format!(
            r#"*** Add File: {}
+ab
+cd"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        // Verify expected stdout and stderr outputs.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nA {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents, "ab\ncd\n");
    }

    #[tokio::test]
    async fn test_apply_patch_hunks_accept_relative_and_absolute_paths() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().abs();
        let relative_add = dir.path().join("relative-add.txt");
        let absolute_add = dir.path().join("absolute-add.txt");
        let relative_delete = dir.path().join("relative-delete.txt");
        let absolute_delete = dir.path().join("absolute-delete.txt");
        let relative_update = dir.path().join("relative-update.txt");
        let absolute_update = dir.path().join("absolute-update.txt");
        fs::write(&relative_delete, "delete relative\n").unwrap();
        fs::write(&absolute_delete, "delete absolute\n").unwrap();
        fs::write(&relative_update, "relative old\n").unwrap();
        fs::write(&absolute_update, "absolute old\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Add File: relative-add.txt
+relative add
*** Add File: {}
+absolute add
*** Delete File: relative-delete.txt
*** Delete File: {}
*** Update File: relative-update.txt
@@
-relative old
+relative new
*** Update File: {}
@@
-absolute old
+absolute new"#,
            absolute_add.display(),
            absolute_delete.display(),
            absolute_update.display(),
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        apply_patch(&patch, &cwd, &mut stdout, &mut stderr)
            .await
            .unwrap();

        assert_eq!(fs::read_to_string(&relative_add).unwrap(), "relative add\n");
        assert_eq!(fs::read_to_string(&absolute_add).unwrap(), "absolute add\n");
        assert!(!relative_delete.exists());
        assert!(!absolute_delete.exists());
        assert_eq!(
            fs::read_to_string(&relative_update).unwrap(),
            "relative new\n"
        );
        assert_eq!(
            fs::read_to_string(&absolute_update).unwrap(),
            "absolute new\n"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Success. Updated the following files:\nA relative-add.txt\nA {}\nM relative-update.txt\nM {}\nD relative-delete.txt\nD {}\n",
                absolute_add.display(),
                absolute_update.display(),
                absolute_delete.display(),
            )
        );
    }

    #[tokio::test]
    async fn test_delete_file_hunk_removes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("del.txt");
        fs::write(&path, "x").unwrap();
        let patch = wrap_patch(&format!("*** Delete File: {}", path.display()));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nD {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_update_file_hunk_modifies_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("update.txt");
        fs::write(&path, "foo\nbar\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+baz"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        // Validate modified file contents and expected stdout/stderr.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "foo\nbaz\n");
    }

    #[tokio::test]
    async fn test_update_file_hunk_can_move_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("dst.txt");
        fs::write(&src, "line\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
*** Move to: {}
@@
-line
+line2"#,
            src.display(),
            dest.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        // Validate move semantics and expected stdout/stderr.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            dest.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        assert!(!src.exists());
        let contents = fs::read_to_string(&dest).unwrap();
        assert_eq!(contents, "line2\n");
    }

    /// Verify that a single `Update File` hunk with multiple change chunks can update different
    /// parts of a file and that the file is listed only once in the summary.
    #[tokio::test]
    async fn test_multiple_update_chunks_apply_to_single_file() {
        // Start with a file containing four lines.
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        fs::write(&path, "foo\nbar\nbaz\nqux\n").unwrap();
        // Construct an update patch with two separate change chunks.
        // The first chunk uses the line `foo` as context and transforms `bar` into `BAR`.
        // The second chunk uses `baz` as context and transforms `qux` into `QUX`.
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+BAR
@@
 baz
-qux
+QUX"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "foo\nBAR\nbaz\nQUX\n");
    }

    /// A more involved `Update File` hunk that exercises additions, deletions and
    /// replacements in separate chunks that appear in non‑adjacent parts of the
    /// file.  Verifies that all edits are applied and that the summary lists the
    /// file only once.
    #[tokio::test]
    async fn test_update_file_hunk_interleaved_changes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interleaved.txt");

        // Original file: six numbered lines.
        fs::write(&path, "a\nb\nc\nd\ne\nf\n").unwrap();

        // Patch performs:
        //  • Replace `b` → `B`
        //  • Replace `e` → `E` (using surrounding context)
        //  • Append new line `g` at the end‑of‑file
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 a
-b
+B
@@
 c
 d
-e
+E
@@
 f
+g
*** End of File"#,
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "a\nB\nc\nd\nE\nf\ng\n");
    }

    #[tokio::test]
    async fn test_pure_addition_chunk_followed_by_removal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("panic.txt");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
+after-context
+second-line
@@
 line1
-line2
-line3
+line2-replacement"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(
            contents,
            "line1\nline2-replacement\nafter-context\nsecond-line\n"
        );
    }

    /// Ensure that patches authored with ASCII characters can update lines that
    /// contain typographic Unicode punctuation (e.g. EN DASH, NON-BREAKING
    /// HYPHEN). Historically `git apply` succeeds in such scenarios but our
    /// internal matcher failed requiring an exact byte-for-byte match.  The
    /// fuzzy-matching pass that normalises common punctuation should now bridge
    /// the gap.
    #[tokio::test]
    async fn test_update_line_with_unicode_dash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unicode.py");

        // Original line contains EN DASH (\u{2013}) and NON-BREAKING HYPHEN (\u{2011}).
        let original = "import asyncio  # local import \u{2013} avoids top\u{2011}level dep\n";
        std::fs::write(&path, original).unwrap();

        // Patch uses plain ASCII dash / hyphen.
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
-import asyncio  # local import - avoids top-level dep
+import asyncio  # HELLO"#,
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();

        // File should now contain the replaced comment.
        let expected = "import asyncio  # HELLO\n";
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, expected);

        // Ensure success summary lists the file as modified.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);

        // No stderr expected.
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[tokio::test]
    async fn test_unified_diff() {
        // Start with a file containing four lines.
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        fs::write(&path, "foo\nbar\nbaz\nqux\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+BAR
@@
 baz
-qux
+QUX"#,
            path.display()
        ));
        let patch = parse_patch(&patch).unwrap();

        let update_file_chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };
        let path_abs = path.as_path().abs();
        let diff = unified_diff_from_chunks(
            &path_abs,
            update_file_chunks,
            LOCAL_FS.as_ref(),
            /*sandbox*/ None,
        )
        .await
        .unwrap();
        let expected_diff = r#"@@ -1,4 +1,4 @@
 foo
-bar
+BAR
 baz
-qux
+QUX
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "foo\nBAR\nbaz\nQUX\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[tokio::test]
    async fn test_unified_diff_first_line_replacement() {
        // Replace the very first line of the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("first.txt");
        fs::write(&path, "foo\nbar\nbaz\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
-foo
+FOO
 bar
"#,
            path.display()
        ));

        let patch = parse_patch(&patch).unwrap();
        let chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let path_abs = path.as_path().abs();
        let diff =
            unified_diff_from_chunks(&path_abs, chunks, LOCAL_FS.as_ref(), /*sandbox*/ None)
                .await
                .unwrap();
        let expected_diff = r#"@@ -1,2 +1,2 @@
-foo
+FOO
 bar
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "FOO\nbar\nbaz\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[tokio::test]
    async fn test_unified_diff_last_line_replacement() {
        // Replace the very last line of the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("last.txt");
        fs::write(&path, "foo\nbar\nbaz\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
 bar
-baz
+BAZ
"#,
            path.display()
        ));

        let patch = parse_patch(&patch).unwrap();
        let chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let path_abs = path.as_path().abs();
        let diff =
            unified_diff_from_chunks(&path_abs, chunks, LOCAL_FS.as_ref(), /*sandbox*/ None)
                .await
                .unwrap();
        let expected_diff = r#"@@ -2,2 +2,2 @@
 bar
-baz
+BAZ
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "foo\nbar\nBAZ\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[tokio::test]
    async fn test_unified_diff_insert_at_eof() {
        // Insert a new line at end‑of‑file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("insert.txt");
        fs::write(&path, "foo\nbar\nbaz\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
+quux
*** End of File
"#,
            path.display()
        ));

        let patch = parse_patch(&patch).unwrap();
        let chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let path_abs = path.as_path().abs();
        let diff =
            unified_diff_from_chunks(&path_abs, chunks, LOCAL_FS.as_ref(), /*sandbox*/ None)
                .await
                .unwrap();
        let expected_diff = r#"@@ -3 +3,2 @@
 baz
+quux
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "foo\nbar\nbaz\nquux\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[tokio::test]
    async fn test_unified_diff_interleaved_changes() {
        // Original file with six lines.
        let dir = tempdir().unwrap();
        let path = dir.path().join("interleaved.txt");
        fs::write(&path, "a\nb\nc\nd\ne\nf\n").unwrap();

        // Patch replaces two separate lines and appends a new one at EOF using
        // three distinct chunks.
        let patch_body = format!(
            r#"*** Update File: {}
@@
 a
-b
+B
@@
 d
-e
+E
@@
 f
+g
*** End of File"#,
            path.display()
        );
        let patch = wrap_patch(&patch_body);

        // Extract chunks then build the unified diff.
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match parsed.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let path_abs = path.as_path().abs();
        let diff =
            unified_diff_from_chunks(&path_abs, chunks, LOCAL_FS.as_ref(), /*sandbox*/ None)
                .await
                .unwrap();

        let expected_diff = r#"@@ -1,6 +1,7 @@
 a
-b
+B
 c
 d
-e
+E
 f
+g
"#;

        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "a\nB\nc\nd\nE\nf\ng\n".to_string(),
        };

        assert_eq!(expected, diff);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(
            contents,
            r#"a
B
c
d
E
f
g
"#
        );
    }

    #[tokio::test]
    async fn test_apply_patch_fails_on_write_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("readonly.txt");
        fs::write(
            &path, "before
",
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        let patch = wrap_patch(&format!(
            "*** Update File: {}
@@
-before
+after
*** End Patch",
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch(
            &patch,
            &AbsolutePathBuf::from_absolute_path(dir.path()).unwrap(),
            &mut stdout,
            &mut stderr,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before
"
        );
        assert_eq!(String::from_utf8(stdout).unwrap(), "");
        let _ = String::from_utf8(stderr).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_move_failure_rolls_back_destination() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let source_dir = dir.path().join("source");
        let dest_dir = dir.path().join("dest");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&dest_dir).unwrap();

        let source_path = source_dir.join("original.txt");
        let dest_path = dest_dir.join("renamed.txt");
        fs::write(
            &source_path,
            "before
",
        )
        .unwrap();
        fs::write(
            &dest_path,
            "existing
",
        )
        .unwrap();

        let original_mode = fs::metadata(&source_dir).unwrap().permissions().mode();
        let mut readonly_dir_permissions = fs::metadata(&source_dir).unwrap().permissions();
        readonly_dir_permissions.set_mode(0o555);
        fs::set_permissions(&source_dir, readonly_dir_permissions).unwrap();

        let patch = wrap_patch(&format!(
            "*** Update File: {}
*** Move to: {}
@@
-before
+after
*** End Patch",
            source_path.display(),
            dest_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr);

        fs::set_permissions(&source_dir, fs::Permissions::from_mode(original_mode)).unwrap();

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&source_path).unwrap(),
            "before
"
        );
        assert_eq!(
            fs::read_to_string(&dest_path).unwrap(),
            "existing
"
        );
        assert_eq!(String::from_utf8(stdout).unwrap(), "");
        let _ = String::from_utf8(stderr).unwrap();
    }

    #[test]
    fn test_apply_patch_add_then_update_same_path_succeeds() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("step.txt");
        let patch = wrap_patch(&format!(
            "*** Add File: {}
+one
*** Update File: {}
@@
-one
+two",
            path.display(),
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "two
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[test]
    fn test_apply_patch_update_then_update_same_path_succeeds() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("double-update.txt");
        fs::write(
            &path, "before
",
        )
        .unwrap();
        let patch = wrap_patch(&format!(
            "*** Update File: {}
@@
-before
+middle
*** Update File: {}
@@
-middle
+after",
            path.display(),
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "after
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[test]
    fn test_apply_patch_move_then_update_destination_succeeds() {
        let dir = tempdir().unwrap();
        let source_path = dir.path().join("source.txt");
        let dest_path = dir.path().join("dest.txt");
        fs::write(
            &source_path,
            "before
",
        )
        .unwrap();
        let patch = wrap_patch(&format!(
            "*** Update File: {}
*** Move to: {}
@@
-before
+middle
*** Update File: {}
@@
-middle
+after",
            source_path.display(),
            dest_path.display(),
            dest_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert!(!source_path.exists());
        assert_eq!(
            fs::read_to_string(&dest_path).unwrap(),
            "after
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_add_over_existing_symlink_keeps_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target_path = dir.path().join("target.txt");
        let link_path = dir.path().join("link.txt");
        fs::write(
            &target_path,
            "before
",
        )
        .unwrap();
        symlink("target.txt", &link_path).unwrap();

        let patch = wrap_patch(&format!(
            "*** Add File: {}
+after",
            link_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert!(
            fs::symlink_metadata(&link_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(&target_path).unwrap(),
            "after
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_update_hardlink_preserves_linked_contents() {
        let dir = tempdir().unwrap();
        let original_path = dir.path().join("original.txt");
        let linked_path = dir.path().join("linked.txt");
        fs::write(
            &original_path,
            "before
",
        )
        .unwrap();
        fs::hard_link(&original_path, &linked_path).unwrap();

        let patch = wrap_patch(&format!(
            "*** Update File: {}
@@
-before
+after",
            linked_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert_eq!(
            fs::read_to_string(&original_path).unwrap(),
            "after
"
        );
        assert_eq!(
            fs::read_to_string(&linked_path).unwrap(),
            "after
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_late_failure_removes_empty_dirs_created_by_prior_add() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let source_dir = dir.path().join("source");
        let dest_dir = dir.path().join("dest");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&dest_dir).unwrap();

        let source_path = source_dir.join("original.txt");
        let dest_path = dest_dir.join("renamed.txt");
        let added_path = dir.path().join("nested/new.txt");
        fs::write(
            &source_path,
            "before
",
        )
        .unwrap();
        fs::write(
            &dest_path,
            "existing
",
        )
        .unwrap();

        let original_mode = fs::metadata(&source_dir).unwrap().permissions().mode();
        let mut readonly_dir_permissions = fs::metadata(&source_dir).unwrap().permissions();
        readonly_dir_permissions.set_mode(0o555);
        fs::set_permissions(&source_dir, readonly_dir_permissions).unwrap();

        let patch = wrap_patch(&format!(
            "*** Add File: {}
+created
*** Update File: {}
*** Move to: {}
@@
-before
+after",
            added_path.display(),
            source_path.display(),
            dest_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr);

        fs::set_permissions(&source_dir, fs::Permissions::from_mode(original_mode)).unwrap();

        assert!(result.is_err());
        assert!(!added_path.exists());
        assert!(!dir.path().join("nested").exists());
        assert_eq!(
            fs::read_to_string(&source_path).unwrap(),
            "before
"
        );
        assert_eq!(
            fs::read_to_string(&dest_path).unwrap(),
            "existing
"
        );
        assert_eq!(String::from_utf8(stdout).unwrap(), "");
        let _ = String::from_utf8(stderr).unwrap();
    }

    #[test]
    fn test_write_new_path_atomically_rename_failure_cleans_temp_file() {
        let dir = tempdir().unwrap();
        let existing_dir = dir.path().join("existing");
        fs::create_dir(&existing_dir).unwrap();

        let error = write_new_path_atomically(
            &existing_dir,
            b"hello
",
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::IsADirectory
        ));
        let leftover_temp = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .find(|name| name.starts_with(".existing.codex-apply-patch."));
        assert_eq!(leftover_temp, None);
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_move_with_hardlinked_source_fails_before_commit() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempdir().unwrap();
        let source_path = dir.path().join("source.txt");
        let dest_path = dir.path().join("dest.txt");
        fs::write(
            &source_path,
            "before
",
        )
        .unwrap();
        fs::hard_link(&source_path, &dest_path).unwrap();

        let patch = wrap_patch(&format!(
            "*** Update File: {}
*** Move to: {}
@@
-before
+after",
            source_path.display(),
            dest_path.display(),
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr);

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&source_path).unwrap(),
            "before
"
        );
        assert_eq!(
            fs::read_to_string(&dest_path).unwrap(),
            "before
"
        );
        let source_metadata = fs::metadata(&source_path).unwrap();
        let dest_metadata = fs::metadata(&dest_path).unwrap();
        assert_eq!(source_metadata.dev(), dest_metadata.dev());
        assert_eq!(source_metadata.ino(), dest_metadata.ino());
        assert_eq!(source_metadata.nlink(), 2);
        assert_eq!(dest_metadata.nlink(), 2);
        assert_eq!(String::from_utf8(stdout).unwrap(), "");
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("multiply-linked regular files are not supported")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_delete_hardlinked_file_fails_before_commit() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempdir().unwrap();
        let source_path = dir.path().join("source.txt");
        let peer_path = dir.path().join("peer.txt");
        fs::write(
            &source_path,
            "before
",
        )
        .unwrap();
        fs::hard_link(&source_path, &peer_path).unwrap();

        let patch = wrap_patch(&format!("*** Delete File: {}", source_path.display()));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr);

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&source_path).unwrap(),
            "before
"
        );
        assert_eq!(
            fs::read_to_string(&peer_path).unwrap(),
            "before
"
        );
        let source_metadata = fs::metadata(&source_path).unwrap();
        let peer_metadata = fs::metadata(&peer_path).unwrap();
        assert_eq!(source_metadata.dev(), peer_metadata.dev());
        assert_eq!(source_metadata.ino(), peer_metadata.ino());
        assert_eq!(source_metadata.nlink(), 2);
        assert_eq!(peer_metadata.nlink(), 2);
        assert_eq!(String::from_utf8(stdout).unwrap(), "");
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("multiply-linked regular files are not supported")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_update_matches_std_write_permission_drop() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let patch_path = dir.path().join("patched.txt");
        let baseline_path = dir.path().join("baseline.txt");
        fs::write(
            &patch_path,
            "before
",
        )
        .unwrap();
        fs::write(
            &baseline_path,
            "before
",
        )
        .unwrap();

        for current_path in [&patch_path, &baseline_path] {
            let mut permissions = fs::metadata(current_path).unwrap().permissions();
            permissions.set_mode(0o4755);
            fs::set_permissions(current_path, permissions).unwrap();
        }

        fs::write(
            &baseline_path,
            b"after
",
        )
        .unwrap();
        let baseline_mode = fs::metadata(&baseline_path).unwrap().permissions().mode() & 0o7777;

        let patch = wrap_patch(&format!(
            "*** Update File: {}
@@
-before
+after",
            patch_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        let patched_mode = fs::metadata(&patch_path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(patched_mode, baseline_mode);
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_delete_dangling_symlink_succeeds() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let path = dir.path().join("dangling-link");
        symlink("missing-target", &path).unwrap();
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let patch = wrap_patch(&format!("*** Delete File: {}", path.display()));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert!(matches!(
            fs::symlink_metadata(&path).map_err(|err| err.kind()),
            Err(std::io::ErrorKind::NotFound)
        ));
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_patch_delete_rollback_restores_setuid_mode_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let delete_path = dir.path().join("setuid.txt");
        let readonly_path = dir.path().join("readonly.txt");
        fs::write(
            &delete_path,
            "before
",
        )
        .unwrap();
        fs::write(
            &readonly_path,
            "locked
",
        )
        .unwrap();
        let mut delete_permissions = fs::metadata(&delete_path).unwrap().permissions();
        delete_permissions.set_mode(0o4755);
        fs::set_permissions(&delete_path, delete_permissions).unwrap();
        let original_mode = fs::metadata(&delete_path).unwrap().permissions().mode() & 0o7777;

        let mut readonly_permissions = fs::metadata(&readonly_path).unwrap().permissions();
        readonly_permissions.set_mode(0o444);
        fs::set_permissions(&readonly_path, readonly_permissions).unwrap();

        let patch = wrap_patch(&format!(
            "*** Delete File: {}
*** Update File: {}
@@
-locked
+changed",
            delete_path.display(),
            readonly_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr);

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&delete_path).unwrap(),
            "before
"
        );
        let restored_mode = fs::metadata(&delete_path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(restored_mode, original_mode);
        assert_eq!(String::from_utf8(stdout).unwrap(), "");
        let _ = String::from_utf8(stderr).unwrap();
    }

    #[test]
    fn test_apply_patch_delete_non_utf8_file_succeeds() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        fs::write(&path, [0xff]).unwrap();
        let patch = wrap_patch(&format!("*** Delete File: {}", path.display()));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert!(!path.exists());
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[test]
    fn test_apply_patch_add_over_existing_non_utf8_file_succeeds() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        fs::write(&path, [0xff]).unwrap();
        let patch = wrap_patch(&format!(
            "*** Add File: {}
+after",
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert_eq!(
            fs::read(&path).unwrap(),
            b"after
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[test]
    fn test_apply_patch_move_over_existing_non_utf8_destination_succeeds() {
        let dir = tempdir().unwrap();
        let source_path = dir.path().join("source.txt");
        let dest_path = dir.path().join("dest.dat");
        fs::write(
            &source_path,
            "before
",
        )
        .unwrap();
        fs::write(&dest_path, [0xff]).unwrap();
        let patch = wrap_patch(&format!(
            "*** Update File: {}
*** Move to: {}
@@
-before
+after",
            source_path.display(),
            dest_path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch_for_test(&patch, dir.path(), &mut stdout, &mut stderr).unwrap();

        assert!(!source_path.exists());
        assert_eq!(
            fs::read(&dest_path).unwrap(),
            b"after
"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }
}
