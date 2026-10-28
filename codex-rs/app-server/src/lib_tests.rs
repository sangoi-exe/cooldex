#![cfg(test)]

//! Unit tests for the crate and its related test suites.

use super::AppServerLaunchMode;
use super::AppServerRuntimeOptions;
use super::AppServerTransport;
use super::ConfigManager;
use super::LogFormat;
use super::ShutdownAction;
use super::ShutdownSignal;
use super::ShutdownState;
use super::app_server_startup_lock_path_for_runtime;
use super::daemon_recovery_file_path_for_runtime;
#[cfg(debug_assertions)]
use super::loader_overrides_with_test_user_config_file;
use super::turn_admission::TurnAdmission;
use super::validate_app_server_launch_mode;
use codex_config::LoaderOverrides;
use codex_config::types::AppServerMode;
use codex_install_context::LocalPackageLane;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

#[test]
fn shutdown_waits_for_admitted_requests_but_force_remains_available() {
    let admission = TurnAdmission::default();
    let active = admission.subscribe_active();
    let in_flight = admission.admit().expect("request admitted");
    let mut shutdown = ShutdownState::default();
    let connection_count = 1;
    let running_turn_count = 0;
    shutdown.on_signal(
        ShutdownSignal::Forceable,
        connection_count,
        running_turn_count,
        &admission,
    );
    assert!(matches!(
        shutdown.update(running_turn_count, *active.borrow(), connection_count),
        ShutdownAction::Noop
    ));
    shutdown.on_signal(
        ShutdownSignal::Forceable,
        connection_count,
        running_turn_count,
        &admission,
    );
    assert!(shutdown.forced());
    assert!(matches!(
        shutdown.update(running_turn_count, *active.borrow(), connection_count),
        ShutdownAction::Finish
    ));
    drop(in_flight);
}

#[test]
fn log_format_from_env_value_matches_json_values_case_insensitively() {
    assert_eq!(LogFormat::from_env_value(Some("json")), LogFormat::Json);
    assert_eq!(LogFormat::from_env_value(Some("JSON")), LogFormat::Json);
    assert_eq!(LogFormat::from_env_value(Some("  Json  ")), LogFormat::Json);
}

#[test]
fn log_format_from_env_value_defaults_for_non_json_values() {
    assert_eq!(
        LogFormat::from_env_value(/*value*/ None),
        LogFormat::Default
    );
    assert_eq!(LogFormat::from_env_value(Some("")), LogFormat::Default);
    assert_eq!(LogFormat::from_env_value(Some("text")), LogFormat::Default);
    assert_eq!(LogFormat::from_env_value(Some("jsonl")), LogFormat::Default);
}

#[tokio::test]
async fn startup_config_reports_defaults_without_recovering_a_missing_selected_profile() {
    let home = tempfile::tempdir().expect("home");
    let config_file = home.path().join("config.toml");
    std::fs::write(&config_file, "").expect("empty config");
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let startup = manager
        .load_startup_config(/*fallback_cwd*/ None)
        .await
        .expect("valid config");
    assert!(!startup.used_defaults);

    std::fs::write(&config_file, "[tui").expect("malformed config");
    let startup = manager
        .load_startup_config(/*fallback_cwd*/ None)
        .await
        .expect("ordinary config errors may use defaults");
    assert!(startup.used_defaults);

    std::fs::write(&config_file, "").expect("restore base config");
    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(
        AbsolutePathBuf::from_absolute_path(home.path().join("missing.config.toml"))
            .expect("selected config path"),
    );
    overrides.user_config_profile = Some("missing".parse().expect("profile name"));
    let manager = ConfigManager::new_for_tests(
        home.path().to_path_buf(),
        Vec::new(),
        overrides,
        Default::default(),
    );
    let error = manager
        .load_startup_config(/*fallback_cwd*/ None)
        .await
        .err()
        .expect("a missing selected profile must not use defaults");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(super::is_nonrecoverable_config_error(&error));
}

#[test]
fn managed_recovery_paths_follow_the_runtime_local_package_lane() {
    let home = std::env::temp_dir().join("codex-app-server-recovery-paths");
    assert_eq!(
        daemon_recovery_file_path_for_runtime(&home, &AppServerRuntimeOptions::default()),
        codex_app_server_transport::daemon_recovery_file_path(&home)
    );
    for local_package_lane in [LocalPackageLane::Codex, LocalPackageLane::CdxDev] {
        assert_eq!(
            daemon_recovery_file_path_for_runtime(
                &home,
                &AppServerRuntimeOptions {
                    local_package_lane: Some(local_package_lane),
                    ..Default::default()
                },
            ),
            codex_app_server_transport::daemon_recovery_file_path_for_local_package_lane(
                &home,
                local_package_lane,
            )
        );
    }
}

#[test]
fn startup_lock_uses_a_lane_lock_only_for_its_canonical_socket() {
    let home = std::env::temp_dir().join("codex-app-server-startup-lock-paths");
    let explicit_unix = AppServerTransport::UnixSocket {
        socket_path: AbsolutePathBuf::from_absolute_path(home.join("explicit.sock"))
            .expect("explicit socket"),
    };
    assert_eq!(
        app_server_startup_lock_path_for_runtime(
            &home,
            &explicit_unix,
            &AppServerRuntimeOptions {
                local_package_lane: Some(LocalPackageLane::Codex),
                ..Default::default()
            },
        )
        .expect("generic startup lock"),
        codex_app_server_transport::app_server_startup_lock_path(&home)
            .expect("expected generic startup lock")
    );

    for local_package_lane in [LocalPackageLane::Codex, LocalPackageLane::CdxDev] {
        let local_socket_path =
            codex_app_server_transport::app_server_control_socket_path_for_local_package_lane(
                &home,
                local_package_lane,
            )
            .expect("lane socket");
        assert_eq!(
            app_server_startup_lock_path_for_runtime(
                &home,
                &AppServerTransport::UnixSocket {
                    socket_path: local_socket_path,
                },
                &AppServerRuntimeOptions {
                    local_package_lane: Some(local_package_lane),
                    ..Default::default()
                },
            )
            .expect("lane startup lock"),
            codex_app_server_transport::app_server_startup_lock_path_for_local_package_lane(
                &home,
                local_package_lane,
            )
            .expect("expected lane startup lock")
        );
    }
}

#[test]
fn launch_mode_validation_keeps_direct_upstream_behavior() {
    validate_app_server_launch_mode(
        AppServerMode::Upstream,
        AppServerLaunchMode::Direct,
        &AppServerTransport::Stdio,
    )
    .expect("direct upstream app-server should remain valid");
}

#[test]
fn launch_mode_validation_rejects_direct_instance_child_config() {
    let err = validate_app_server_launch_mode(
        AppServerMode::InstanceChild,
        AppServerLaunchMode::Direct,
        &AppServerTransport::Stdio,
    )
    .expect_err("direct launch should reject instance-child config");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("ordinary interactive TUI"));
}

#[test]
fn launch_mode_validation_rejects_mismatched_marker_and_transport() {
    let mode_err = validate_app_server_launch_mode(
        AppServerMode::Upstream,
        AppServerLaunchMode::InstanceChild,
        &AppServerTransport::Stdio,
    )
    .expect_err("internal marker should require matching config");
    assert!(
        mode_err
            .to_string()
            .contains("requires `tui.app_server_mode")
    );

    let transport_err = validate_app_server_launch_mode(
        AppServerMode::InstanceChild,
        AppServerLaunchMode::InstanceChild,
        &AppServerTransport::Stdio,
    )
    .expect_err("internal marker should require Unix transport");
    assert!(transport_err.to_string().contains("Unix socket"));
}

#[cfg(debug_assertions)]
#[test]
fn debug_test_user_config_file_overrides_loader_path() {
    let path = std::env::temp_dir().join("codex-app-server-test-config.toml");
    let loader_overrides =
        loader_overrides_with_test_user_config_file(LoaderOverrides::default(), Some(path.clone()))
            .expect("test config path should be valid");

    assert_eq!(
        loader_overrides.user_config_path,
        Some(AbsolutePathBuf::from_absolute_path(path).expect("absolute test path"))
    );
}

#[path = "stderr_logging_tests.rs"]
mod stderr_logging_tests;
