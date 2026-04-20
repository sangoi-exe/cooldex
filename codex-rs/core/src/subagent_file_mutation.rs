use crate::config::Config;
use codex_protocol::config_types::SubagentFileMutationMode;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::SandboxPermissions;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::request_permissions::RequestPermissionProfile;

// Merge-safety anchor: this module is the single owner for spawn-only child file-mutation denial semantics across policy downgrade and fail-loud tool gating.

pub(crate) const FILE_MUTATION_DENIED_PREFIX: &str =
    "spawned agent profile forbids filesystem mutation";

pub(crate) fn file_mutation_is_denied(config: &Config) -> bool {
    matches!(
        config.subagent_file_mutation_mode,
        SubagentFileMutationMode::Deny
    )
}

pub(crate) fn denied_action_message(action: &str) -> String {
    format!("{FILE_MUTATION_DENIED_PREFIX}; {action}")
}

pub(crate) fn permission_profile_requests_file_system_write(
    profile: Option<&PermissionProfile>,
) -> bool {
    profile
        .and_then(|profile| profile.file_system.as_ref())
        .is_some_and(|file_system| file_system.write.is_some())
}

pub(crate) fn request_permission_profile_requests_file_system_write(
    profile: &RequestPermissionProfile,
) -> bool {
    profile
        .file_system
        .as_ref()
        .is_some_and(|file_system| file_system.write.is_some())
}

pub(crate) fn shell_request_widens_file_mutation(
    sandbox_permissions: SandboxPermissions,
    additional_permissions: Option<&PermissionProfile>,
) -> bool {
    sandbox_permissions.requires_escalated_permissions()
        || permission_profile_requests_file_system_write(additional_permissions)
}

pub(crate) fn apply_file_mutation_mode_to_config(
    config: &mut Config,
    mode: SubagentFileMutationMode,
) -> Result<(), String> {
    config.subagent_file_mutation_mode = mode;
    if !matches!(mode, SubagentFileMutationMode::Deny) {
        return Ok(());
    }

    let read_only_file_system_policy =
        deny_file_mutation_policy(&config.permissions.file_system_sandbox_policy);
    let read_only_sandbox_policy = read_only_file_system_policy
        .to_legacy_sandbox_policy(
            config.permissions.network_sandbox_policy,
            config.cwd.as_path(),
        )
        .map_err(|err| format!("failed to derive read-only sandbox policy: {err}"))?;
    config
        .permissions
        .sandbox_policy
        .set(read_only_sandbox_policy)
        .map_err(|err| format!("sandbox_policy is invalid: {err}"))?;
    config.permissions.file_system_sandbox_policy = read_only_file_system_policy;
    Ok(())
}

pub(crate) fn restore_file_mutation_mode_to_config(
    config: &mut Config,
    mode: SubagentFileMutationMode,
    sandbox_policy: &SandboxPolicy,
) -> Result<(), String> {
    config
        .permissions
        .sandbox_policy
        .set(sandbox_policy.clone())
        .map_err(|err| format!("sandbox_policy is invalid: {err}"))?;
    config.permissions.file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(sandbox_policy, config.cwd.as_path());
    config.permissions.network_sandbox_policy = NetworkSandboxPolicy::from(sandbox_policy);
    apply_file_mutation_mode_to_config(config, mode)
}

fn deny_file_mutation_policy(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
) -> FileSystemSandboxPolicy {
    match file_system_sandbox_policy.kind {
        FileSystemSandboxKind::Restricted => FileSystemSandboxPolicy::restricted(
            file_system_sandbox_policy
                .entries
                .iter()
                .map(|entry| FileSystemSandboxEntry {
                    path: entry.path.clone(),
                    access: downgrade_write_access(entry.access),
                })
                .collect(),
        ),
        FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => {
            FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
            }])
        }
    }
}

fn downgrade_write_access(access: FileSystemAccessMode) -> FileSystemAccessMode {
    match access {
        FileSystemAccessMode::Write => FileSystemAccessMode::Read,
        FileSystemAccessMode::Read | FileSystemAccessMode::None => access,
    }
}

#[cfg(test)]
mod tests {
    use super::apply_file_mutation_mode_to_config;
    use super::permission_profile_requests_file_system_write;
    use super::request_permission_profile_requests_file_system_write;
    use super::restore_file_mutation_mode_to_config;
    use codex_protocol::config_types::SubagentFileMutationMode;
    use codex_protocol::models::FileSystemPermissions;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSandboxPolicy;
    use codex_protocol::permissions::FileSystemSpecialPath;
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use codex_protocol::protocol::SandboxPolicy;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn apply_file_mutation_mode_to_config_downgrades_write_access_but_keeps_network() {
        let tempdir = TempDir::new().expect("tempdir");
        let cwd = AbsolutePathBuf::from_absolute_path(tempdir.path()).expect("absolute cwd");
        let mut config = crate::config::test_config().await;
        config.cwd = cwd.clone();
        config.permissions.network_sandbox_policy = NetworkSandboxPolicy::Enabled;
        config.permissions.file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::CurrentWorkingDirectory,
                },
                access: FileSystemAccessMode::Write,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Minimal,
                },
                access: FileSystemAccessMode::Read,
            },
        ]);

        apply_file_mutation_mode_to_config(&mut config, SubagentFileMutationMode::Deny)
            .expect("deny mode should apply");

        assert_eq!(
            config.permissions.sandbox_policy.get(),
            &SandboxPolicy::ReadOnly {
                access: codex_protocol::protocol::ReadOnlyAccess::Restricted {
                    include_platform_defaults: true,
                    readable_roots: vec![cwd],
                },
                network_access: true,
            }
        );
        assert!(
            !config
                .permissions
                .file_system_sandbox_policy
                .can_write_path_with_cwd(tempdir.path(), tempdir.path())
        );
    }

    #[test]
    fn file_system_write_detection_only_flags_write_requests() {
        let read_only = PermissionProfile {
            file_system: Some(FileSystemPermissions {
                read: Some(vec![]),
                write: None,
            }),
            ..PermissionProfile::default()
        };
        let write = PermissionProfile {
            file_system: Some(FileSystemPermissions {
                read: None,
                write: Some(vec![]),
            }),
            ..PermissionProfile::default()
        };

        assert!(!permission_profile_requests_file_system_write(Some(
            &read_only
        )));
        assert!(permission_profile_requests_file_system_write(Some(&write)));
        assert!(
            request_permission_profile_requests_file_system_write(&write.into()),
            "request_permissions should detect write requests too"
        );
    }

    #[tokio::test]
    async fn restore_file_mutation_mode_to_config_restores_baseline_before_clearing_deny() {
        let tempdir = TempDir::new().expect("tempdir");
        let cwd = AbsolutePathBuf::from_absolute_path(tempdir.path()).expect("absolute cwd");
        let mut config = crate::config::test_config().await;
        config.cwd = cwd;

        apply_file_mutation_mode_to_config(&mut config, SubagentFileMutationMode::Deny)
            .expect("deny mode should apply");

        restore_file_mutation_mode_to_config(
            &mut config,
            SubagentFileMutationMode::Inherit,
            &SandboxPolicy::DangerFullAccess,
        )
        .expect("restore should clear deny state");

        assert_eq!(
            config.subagent_file_mutation_mode,
            SubagentFileMutationMode::Inherit
        );
        assert_eq!(
            config.permissions.sandbox_policy.get(),
            &SandboxPolicy::DangerFullAccess
        );
        assert!(
            config
                .permissions
                .file_system_sandbox_policy
                .can_write_path_with_cwd(tempdir.path(), tempdir.path())
        );
    }
}
