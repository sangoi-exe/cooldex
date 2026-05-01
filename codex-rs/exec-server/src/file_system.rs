use async_trait::async_trait;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::SandboxEnforcement;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::NetworkAccess;
use codex_protocol::protocol::ReadOnlyAccess;
use codex_protocol::protocol::SandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreateDirectoryOptions {
    pub recursive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoveOptions {
    pub recursive: bool,
    pub force: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyOptions {
    pub recursive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileMetadata {
    pub is_directory: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub created_at_ms: i64,
    pub modified_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadDirectoryEntry {
    pub file_name: String,
    pub is_directory: bool,
    pub is_file: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemSandboxContext {
    pub sandbox_policy: SandboxPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy_cwd: Option<AbsolutePathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_system_sandbox_policy: Option<FileSystemSandboxPolicy>,
    pub windows_sandbox_level: WindowsSandboxLevel,
    #[serde(default)]
    pub windows_sandbox_private_desktop: bool,
    #[serde(default)]
    pub use_legacy_landlock: bool,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
}

impl FileSystemSandboxContext {
    pub fn new(sandbox_policy: SandboxPolicy) -> Self {
        Self {
            sandbox_policy,
            sandbox_policy_cwd: None,
            file_system_sandbox_policy: None,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: false,
            use_legacy_landlock: false,
            additional_permissions: None,
        }
    }

    pub fn from_permission_profile(permission_profile: PermissionProfile) -> Self {
        Self::from_permission_profile_with_optional_cwd(permission_profile, None)
    }

    pub fn from_permission_profile_with_cwd(
        permission_profile: PermissionProfile,
        cwd: AbsolutePathBuf,
    ) -> Self {
        Self::from_permission_profile_with_optional_cwd(permission_profile, Some(cwd))
    }

    fn from_permission_profile_with_optional_cwd(
        permission_profile: PermissionProfile,
        cwd: Option<AbsolutePathBuf>,
    ) -> Self {
        let (file_system_sandbox_policy, network_sandbox_policy) =
            permission_profile.to_runtime_permissions();
        let projection_cwd = cwd
            .as_ref()
            .cloned()
            .or_else(|| AbsolutePathBuf::current_dir().ok());
        let sandbox_policy = projection_cwd
            .as_ref()
            .and_then(|cwd| {
                permission_profile
                    .to_legacy_sandbox_policy(cwd.as_path())
                    .ok()
            })
            .unwrap_or_else(|| {
                fallback_sandbox_policy_for_profile(&permission_profile, network_sandbox_policy)
            });

        Self {
            sandbox_policy,
            sandbox_policy_cwd: cwd,
            file_system_sandbox_policy: Some(file_system_sandbox_policy),
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: false,
            use_legacy_landlock: false,
            additional_permissions: None,
        }
    }

    pub fn should_run_in_sandbox(&self) -> bool {
        matches!(
            self.sandbox_policy,
            SandboxPolicy::ReadOnly { .. } | SandboxPolicy::WorkspaceWrite { .. }
        )
    }
}

fn fallback_sandbox_policy_for_profile(
    permission_profile: &PermissionProfile,
    network_sandbox_policy: NetworkSandboxPolicy,
) -> SandboxPolicy {
    match permission_profile.enforcement() {
        SandboxEnforcement::Managed => SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            read_only_access: ReadOnlyAccess::default(),
            network_access: network_sandbox_policy.is_enabled(),
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        },
        SandboxEnforcement::Disabled => SandboxPolicy::DangerFullAccess,
        SandboxEnforcement::External => SandboxPolicy::ExternalSandbox {
            network_access: if network_sandbox_policy.is_enabled() {
                NetworkAccess::Enabled
            } else {
                NetworkAccess::Restricted
            },
        },
    }
}

pub type FileSystemResult<T> = io::Result<T>;

#[async_trait]
pub trait ExecutorFileSystem: Send + Sync {
    async fn read_file(
        &self,
        path: &AbsolutePathBuf,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<Vec<u8>>;

    /// Reads a file and decodes it as UTF-8 text.
    async fn read_file_text(
        &self,
        path: &AbsolutePathBuf,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<String> {
        let bytes = self.read_file(path, sandbox).await?;
        String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    async fn write_file(
        &self,
        path: &AbsolutePathBuf,
        contents: Vec<u8>,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()>;

    async fn create_directory(
        &self,
        path: &AbsolutePathBuf,
        create_directory_options: CreateDirectoryOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()>;

    async fn get_metadata(
        &self,
        path: &AbsolutePathBuf,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<FileMetadata>;

    async fn read_directory(
        &self,
        path: &AbsolutePathBuf,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<Vec<ReadDirectoryEntry>>;

    async fn remove(
        &self,
        path: &AbsolutePathBuf,
        remove_options: RemoveOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()>;

    async fn copy(
        &self,
        source_path: &AbsolutePathBuf,
        destination_path: &AbsolutePathBuf,
        copy_options: CopyOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()>;
}
