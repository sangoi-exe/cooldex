//! Opportunistic attachment may fall back; automatic startup requires a shared server.
//! Merge-safety anchor: only implicit daemon targets may fall back, while startup consistently
//! returns the ownership-aware AppServerSession for every target mode.

use crate::legacy_core::config::ConfigBuilder;
use crate::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[cfg(unix)]
#[tokio::test]
async fn explicit_local_daemon_recognition_verifies_only_selected_owner() -> color_eyre::Result<()>
{
    let home = TempDir::new()?;
    let mut daemons = Vec::new();
    for daemon_lane in [
        None,
        Some(LocalPackageLane::Codex),
        Some(LocalPackageLane::CdxDev),
    ] {
        let advertised_socket = match daemon_lane {
            Some(lane) => {
                codex_app_server_transport::app_server_control_socket_path_for_local_package_lane(
                    home.path(),
                    lane,
                )?
            }
            None => codex_app_server_client::app_server_control_socket_path(home.path())?,
        };
        let (listener, guard) =
            codex_app_server_transport::bind_private_daemon_socket(advertised_socket.as_path())
                .await?;
        let verified_socket =
            AbsolutePathBuf::from_absolute_path(std::fs::read_link(advertised_socket.as_path())?)?;
        assert_ne!(advertised_socket, verified_socket);
        daemons.push((
            daemon_lane,
            advertised_socket,
            verified_socket,
            listener,
            guard,
        ));
    }

    for caller_lane in [
        None,
        Some(LocalPackageLane::Codex),
        Some(LocalPackageLane::CdxDev),
    ] {
        for (daemon_lane, advertised_socket, verified_socket, _, _) in &daemons {
            let mut endpoint = Some(RemoteAppServerEndpoint::UnixSocket {
                socket_path: advertised_socket.clone(),
            });
            let local_socket = startup_orchestration::resolve_explicit_local_daemon_socket(
                home.path(),
                caller_lane,
                &mut endpoint,
            )?;
            let same_owner = caller_lane == *daemon_lane;
            assert_eq!(local_socket.as_ref(), same_owner.then_some(verified_socket));
            let expected_endpoint = RemoteAppServerEndpoint::UnixSocket {
                socket_path: if same_owner {
                    verified_socket.clone()
                } else {
                    advertised_socket.clone()
                },
            };
            assert_eq!(endpoint.as_ref(), Some(&expected_endpoint));
            let target = app_server_target_for_launch(
                endpoint,
                local_socket,
                /*can_reuse_implicit_local_daemon*/ false,
                AppServerMode::Upstream,
                /*workload_identity_selected*/ false,
                /*exec_server_url*/ None,
            )?;
            if same_owner {
                assert_eq!(
                    target,
                    AppServerTarget::LocalDaemon {
                        endpoint: expected_endpoint,
                        allow_embedded_fallback: false,
                    }
                );
                assert!(!target.uses_remote_workspace());
                assert_eq!(target.thread_params_mode(), ThreadParamsMode::Embedded);
            } else {
                assert_eq!(
                    target,
                    AppServerTarget::Remote {
                        endpoint: expected_endpoint
                    }
                );
                assert!(target.uses_remote_workspace());
                assert_eq!(target.thread_params_mode(), ThreadParamsMode::Remote);
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_local_daemon_recognition_rejects_unverified_advertised_socket()
-> color_eyre::Result<()> {
    let home = TempDir::new()?;
    for caller_lane in [
        None,
        Some(LocalPackageLane::Codex),
        Some(LocalPackageLane::CdxDev),
    ] {
        let socket_path = daemon_socket_path(home.path(), caller_lane)?;
        codex_uds::prepare_private_socket_directory(
            socket_path.as_path().parent().expect("socket parent"),
        )
        .await?;
        // A real listener at the advertised path may be an SSH forward, not our daemon.
        let _listener = codex_uds::UnixListener::bind(socket_path.as_path()).await?;
        let expected_endpoint = RemoteAppServerEndpoint::UnixSocket { socket_path };
        let mut endpoint = Some(expected_endpoint.clone());
        let local_socket = startup_orchestration::resolve_explicit_local_daemon_socket(
            home.path(),
            caller_lane,
            &mut endpoint,
        )?;
        assert_eq!(local_socket, None);
        assert_eq!(endpoint.as_ref(), Some(&expected_endpoint));
        assert_eq!(
            app_server_target_for_launch(
                endpoint,
                local_socket,
                /*can_reuse_implicit_local_daemon*/ false,
                AppServerMode::Upstream,
                /*workload_identity_selected*/ false,
                /*exec_server_url*/ None,
            )?,
            AppServerTarget::Remote {
                endpoint: expected_endpoint
            }
        );
    }
    Ok(())
}

#[test]
fn explicit_local_daemon_recognition_requires_selected_socket_to_exist() -> color_eyre::Result<()> {
    let home = TempDir::new()?;
    for caller_lane in [
        None,
        Some(LocalPackageLane::Codex),
        Some(LocalPackageLane::CdxDev),
    ] {
        let expected_endpoint = RemoteAppServerEndpoint::UnixSocket {
            socket_path: daemon_socket_path(home.path(), caller_lane)?,
        };
        let mut endpoint = Some(expected_endpoint.clone());
        let error = startup_orchestration::resolve_explicit_local_daemon_socket(
            home.path(),
            caller_lane,
            &mut endpoint,
        )
        .expect_err("an explicit local attachment must not become remote during a restart");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(endpoint, Some(expected_endpoint));
    }
    Ok(())
}

#[test]
fn feature_overrides_allow_daemon_without_allowing_arbitrary_config() {
    for (raw, eligible) in [
        ("features.app_server_websocket_compression=true", true),
        ("features.app_server_websocket_compression=false", true),
        ("features.worktrees='true'", false),
        ("features={worktrees=true,shell_tool=false}", true),
        ("suppress_unstable_features_warning=true", true),
        ("suppress_unstable_features_warning='true'", false),
        ("features.shell_tool=false", true),
        ("features.worktrees.enabled=true", false),
        ("tui.fullscreen_transcript=true", true),
        ("tui.fullscreen_transcript=false", true),
        ("tui.fullscreen_transcript=\"true\"", false),
        ("tui={fullscreen_transcript=true}", true),
        ("tui={fullscreen_transcript='true'}", false),
        ("tui={fullscreen_transcript=true,animations=false}", false),
        ("features={}", false),
        ("model='test'", false),
    ] {
        let overrides = codex_utils_cli::CliConfigOverrides {
            raw_overrides: vec![raw.to_string()],
        }
        .parse_overrides()
        .unwrap();
        assert_eq!(
            daemon_startup::config_exclusion(
                &overrides,
                &LoaderOverrides::default(),
                /*strict_config*/ false,
                /*bypass_hook_trust*/ false,
            )
            .is_none(),
            eligible,
            "{raw}"
        );
    }
}

#[test]
fn daemon_launch_telemetry_records_once_on_connection_or_early_return() {
    for connected in [false, true] {
        let observations = std::cell::RefCell::new(Vec::new());
        let launch = daemon_telemetry::Launch(Some(|target: &AppServerTarget, actual: bool| {
            observations.borrow_mut().push((target.clone(), actual));
            Vec::new()
        }));
        if connected {
            launch.record(&AppServerTarget::Embedded, connected);
        } else {
            drop(launch);
        }
        assert_eq!(
            observations.into_inner(),
            vec![(AppServerTarget::Embedded, connected)]
        );
    }
}

#[tokio::test]
async fn daemon_feature_compatibility_respects_required_and_optional_attachment()
-> color_eyre::Result<()> {
    use codex_app_server_protocol::JSONRPCMessage;
    use futures::SinkExt;
    use futures::StreamExt;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;

    for allow_embedded_fallback in [false, true] {
        for scenario in [
            "matching",
            "unreachable daemon",
            "conflicting client",
            "explicit enable",
            "mixed flags",
            "missing enabled",
            "managed",
            "legacy managed",
            "old requirements server",
            "unsupported RPC",
        ] {
            let cli_overrides = codex_utils_cli::CliConfigOverrides {
                raw_overrides: match scenario {
                    "matching" => vec![
                        "features.instant_interrupt=true",
                        "features.remote_compaction_v2=true",
                        "features.shell_tool=true",
                        "features.zz_missing=false",
                    ],
                    "unreachable daemon" => Vec::new(),
                    "explicit enable" => vec!["features.api_key_model_discovery=true"],
                    "mixed flags" => vec![
                        "features.zz_overridden=true",
                        "features={api_key_model_discovery=false,code_mode_host=false,instant_interrupt=true}",
                        "features.code_mode_host=true",
                    ],
                    "missing enabled" => vec!["features.zz_missing=true"],
                    _ => vec!["features.api_key_model_discovery=false"],
                }
                .into_iter()
                .map(str::to_string)
                .collect(),
            }
            .parse_overrides()
            .unwrap();
            let home = TempDir::new()?;
            let config = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
                .build()
                .await?;
            let features = [
                (Feature::ApiKeyModelDiscovery, scenario != "explicit enable", "beta"),
                (Feature::CodeModeHost, false, "beta"),
                (Feature::InstantInterrupt, true, "beta"),
                (Feature::RemoteCompactionV2, false, "stable"),
                (Feature::ShellTool, false, "removed"),
            ].map(|(feature, enabled, stage)| {
                json!({"name": feature.key(), "stage": stage, "enabled": enabled, "defaultEnabled": false})
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let target = AppServerTarget::LocalDaemon {
                endpoint: RemoteAppServerEndpoint::WebSocket {
                    websocket_url: format!("ws://{}", listener.local_addr()?),
                    auth_token: None,
                },
                allow_embedded_fallback,
            };
            if cli_overrides.is_empty() {
                drop(listener);
                let result =
                    daemon_startup::compatibility_warning(&target, &config, &cli_overrides).await;
                assert!(matches!(
                    (allow_embedded_fallback, result),
                    (true, Ok(Some(_))) | (false, Err(_))
                ));
                continue;
            }
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap()
                    else {
                        continue;
                    };
                    let response = match request.method.as_str() {
                        "initialize" => {
                            json!({"id": request.id, "result": {"userAgent": "daemon-test"}})
                        }
                        "config/read" => {
                            json!({"id": request.id, "result": {"config": {}, "layers": [{
                                "name": {"type": "legacyManagedConfigTomlFromFile"}, "config":
                                if scenario == "legacy managed" {
                                    json!({"features": {"api_key_model_discovery": true}})
                                } else { json!({}) }
                            }]}})
                        }
                        "configRequirements/read" if scenario == "old requirements server" => {
                            json!({"id": request.id, "error": {"code": -32600, "message": "Invalid request: unknown variant `configRequirements/read`"}})
                        }
                        "configRequirements/read" => json!({"id": request.id, "result": {
                            "requirements": if scenario == "managed" {
                                json!({"featureRequirements": {"api_key_model_discovery": true}})
                            } else { json!(null) }
                        }}),
                        "experimentalFeature/list" if scenario == "unsupported RPC" => {
                            json!({"id": request.id, "error": {"code": -32601, "message": "method not found"}})
                        }
                        "experimentalFeature/list" => {
                            assert_eq!(request.params.unwrap()["threadId"], json!(null));
                            json!({"id": request.id, "result": {"data": features, "nextCursor": null}})
                        }
                        _ => panic!("unexpected request: {}", request.method),
                    };
                    socket
                        .send(Message::Text(response.to_string().into()))
                        .await
                        .unwrap();
                }
            });
            let result =
                daemon_startup::compatibility_warning(&target, &config, &cli_overrides).await;
            server.await?;
            if matches!(scenario, "matching" | "managed" | "legacy managed") {
                assert_eq!(result?, None);
                continue;
            }
            let reason = match scenario {
                "explicit enable" => "This session requires api_key_model_discovery to be enabled",
                "conflicting client" | "old requirements server" | "mixed flags" => {
                    "This session requires api_key_model_discovery to be disabled"
                }
                "missing enabled" => "This session requires zz_missing to be enabled",
                "unsupported RPC" => "Experimental feature request failed",
                _ => unreachable!(),
            };
            let restartable = scenario != "unsupported RPC";
            if allow_embedded_fallback && !restartable {
                let warning = result?.expect("optional attachment should fall back");
                assert_eq!(
                    warning,
                    format!("Running without the shared background server: {reason}.")
                );
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.restart_features.is_some(), restartable);
                if scenario == "mixed flags" {
                    assert_eq!(
                        error.restart_features,
                        Some(std::collections::BTreeMap::from([
                            ("api_key_model_discovery".to_string(), false),
                            ("code_mode_host".to_string(), true),
                        ]))
                    );
                }
                let error = error.to_string();
                assert_eq!(
                    error,
                    format!(
                        "Cannot use the shared background server: {reason}.\n{}",
                        daemon_startup::FAILURE_HINT
                    )
                );
                if scenario == "explicit enable" && !allow_embedded_fallback {
                    insta::assert_snapshot!("daemon_feature_mismatch_error", error);
                }
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn daemon_connection_rejects_unprotected_socket_before_handshake() -> color_eyre::Result<()> {
    let home = TempDir::new()?;
    let parent = home.path().join("control");
    std::fs::create_dir(&parent)?;
    let socket_path = AbsolutePathBuf::from_absolute_path_checked(parent.join("server.sock"))?;
    let mut listener = codex_uds::UnixListener::bind(socket_path.as_path()).await?;
    let target = AppServerTarget::LocalDaemon {
        allow_embedded_fallback: true,
        endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
    };
    tokio::select! {
        result = app_server_connection::connect(&target) => assert!(result.is_err()),
        _ = listener.accept() => panic!("unprotected listener must not receive a connection"),
    }
    Ok(())
}

#[tokio::test]
async fn daemon_startup_falls_back_only_for_implicit_endpoints() -> color_eyre::Result<()> {
    for scenario in [
        "missing socket",
        "failed handshake",
        "explicit endpoint",
        "required daemon",
    ] {
        let home = TempDir::new()?;
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = if scenario == "missing socket" {
            RemoteAppServerEndpoint::UnixSocket {
                socket_path: AbsolutePathBuf::from_absolute_path_checked(
                    home.path().join("gone.sock"),
                )?,
            }
        } else {
            RemoteAppServerEndpoint::WebSocket {
                websocket_url: format!("ws://{}", listener.local_addr()?),
                auth_token: None,
            }
        };
        let reject_handshake = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let mut target = if scenario == "explicit endpoint" {
            AppServerTarget::Remote { endpoint }
        } else {
            AppServerTarget::LocalDaemon {
                endpoint,
                allow_embedded_fallback: scenario != "required daemon",
            }
        };
        let original_target = target.clone();
        let mut state_db = None;
        let result = start_app_server(
            crate::backend_id::BackendId::new(),
            &mut target,
            Arg0DispatchPaths::default(),
            config,
            Vec::new(),
            Vec::new(),
            LoaderOverrides::default(),
            /*strict_config*/ false,
            CloudConfigBundleLoader::default(),
            codex_feedback::CodexFeedback::new(),
            /*log_db*/ None,
            &mut state_db,
            Arc::new(EnvironmentManager::default_for_tests()),
            Default::default(),
        )
        .await;
        reject_handshake.abort();
        if scenario == "explicit endpoint" || scenario == "required daemon" {
            assert!(result.is_err());
            if scenario == "required daemon" {
                let message = result.err().unwrap().to_string();
                assert!(message.contains("rerun the same command with --no-daemon"));
                assert!(message.contains("failed to connect to remote app server"));
            }
            assert_eq!(target, original_target);
            assert!(state_db.is_none());
        } else {
            let server = result?;
            assert!(server.uses_embedded_app_server());
            assert_eq!(target, AppServerTarget::Embedded);
            assert!(state_db.is_some());
            server.shutdown().await?;
        }
    }
    Ok(())
}

#[test]
fn daemon_eligibility_preserves_launch_options_and_explains_exclusions() {
    use clap::Parser;
    for (args, expected) in [
        ("--no-daemon", Some("--no-daemon")),
        ("--worktree", None),
        ("--worktree --no-daemon", Some("--no-daemon")),
        ("--oss", Some("--oss")),
        ("--profile test", Some("--profile")),
        ("--strict-config", Some("--strict-config")),
        (
            "--dangerously-bypass-hook-trust",
            Some("--dangerously-bypass-hook-trust"),
        ),
        (
            "-m test --cd /tmp -i image.png -a never -s workspace-write --add-dir /tmp --no-alt-screen hello",
            None,
        ),
    ] {
        let cli = Cli::parse_from(std::iter::once("codex").chain(args.split_whitespace()));
        assert_eq!(
            daemon_startup::exclusion(
                &cli,
                &[],
                &LoaderOverrides::default(),
                /*workload_identity_selected*/ false,
                /*exec_server_url*/ None
            ),
            expected
        );
    }
    let mut cli = Cli::parse_from(["codex"]);
    let overrides = vec![("web_search".into(), toml::Value::String("live".into()))];
    let loader = LoaderOverrides {
        ignore_user_config: true,
        ..Default::default()
    };
    for (kv, loader, workload, executor, expected) in [
        (
            &overrides[..],
            LoaderOverrides::default(),
            false,
            None,
            "command-line configuration overrides (-c, --enable, --disable, or --search)",
        ),
        (&[][..], loader, false, None, "custom configuration loader"),
        (
            &[][..],
            LoaderOverrides::default(),
            true,
            None,
            "workload identity",
        ),
        (
            &[][..],
            LoaderOverrides::default(),
            false,
            Some(std::ffi::OsStr::new("executor")),
            "executor selection (CODEX_EXEC_SERVER_URL)",
        ),
    ] {
        assert_eq!(
            daemon_startup::exclusion(&cli, kv, &loader, workload, executor),
            Some(expected)
        );
    }
    cli.agents_overview = true;
    cli.strict_config = true;
    assert_eq!(
        daemon_startup::exclusion(
            &cli,
            &overrides,
            &LoaderOverrides::default(),
            /*workload_identity_selected*/ false,
            /*exec_server_url*/ None
        ),
        None
    );
}

#[test]
fn daemon_exclusion_warning_snapshot() {
    use crate::history_cell::HistoryCell;
    let cell = crate::history_cell::StartupWarningsCell::new(vec![
        "Running without the shared background server: --strict-config requires embedded mode."
            .into(),
        format!(
            "Running without the shared background server: {} requires embedded mode.",
            daemon_startup::WSL_DRVFS_EXCLUSION
        ),
    ]);
    let text = cell
        .transcript_lines(/*width*/ 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("daemon_exclusion_warning", text);
}
