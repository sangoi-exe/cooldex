//! Bounded observations for foreground callers. This module never exports telemetry;
//! in particular, the detached updater must not create a telemetry provider.

use std::path::Path;

use codex_install_context::LocalPackageLane;

/// Suppress child reporting when the foreground TUI handoff owns the observation.
pub const HANDOFF_ENV: &str = "CODEX_DAEMON_TELEMETRY_HANDOFF";

/// Only presence and enabled state leave the settings reader, never values or contents.
/// Missing files mean defaults; unreadable or invalid settings remain unknown.
pub async fn settings_tags(codex_home: &Path) -> [(&'static str, &'static str); 4] {
    settings_tags_for_path(
        codex_home
            .join(crate::STATE_DIR_NAME)
            .join(crate::SETTINGS_FILE_NAME),
    )
    .await
}

/// Reads daemon telemetry settings for one typed local package lane.
pub async fn settings_tags_for_local_package_lane(
    codex_home: &Path,
    local_package_lane: LocalPackageLane,
) -> [(&'static str, &'static str); 4] {
    settings_tags_for_path(
        codex_home
            .join(crate::STATE_DIR_NAME)
            .join(local_package_lane.package_root_component())
            .join(crate::SETTINGS_FILE_NAME),
    )
    .await
}

// Merge-safety anchor: foreground telemetry receives a resolved typed package lane here so local packages read only their own daemon settings while unbound execution retains the generic settings path.
/// Reads daemon telemetry settings for a local package lane, or generic settings when unbound.
pub async fn settings_tags_for_local_package_lane_or_default(
    codex_home: &Path,
    local_package_lane: Option<LocalPackageLane>,
) -> [(&'static str, &'static str); 4] {
    match local_package_lane {
        Some(local_package_lane) => {
            settings_tags_for_local_package_lane(codex_home, local_package_lane).await
        }
        None => settings_tags(codex_home).await,
    }
}

async fn settings_tags_for_path(path: impl AsRef<Path>) -> [(&'static str, &'static str); 4] {
    crate::settings::telemetry_tags(path.as_ref())
        .await
        .unwrap_or([
            ("auto_update", "unknown"),
            ("auto_update_setting", "unknown"),
            ("update_interval_setting", "unknown"),
            ("shutdown_grace_setting", "unknown"),
        ])
}
