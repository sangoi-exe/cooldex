use super::AppServerArgs;
use super::transport_for_listen_url;
use clap::Parser;
use codex_app_server::AppServerTransport;
use codex_install_context::LocalPackageLane;
use pretty_assertions::assert_eq;
use toml::Value as TomlValue;
use url::Url;

#[test]
fn app_server_accepts_cli_config_overrides() {
    let args = AppServerArgs::try_parse_from([
        "codex-app-server",
        "-c",
        "model=\"gpt-5-codex\"",
        "--config",
        "sandbox_mode=\"read-only\"",
        "--listen",
        "off",
    ])
    .expect("parse app-server args");

    let parsed_overrides = args
        .config_overrides
        .parse_overrides()
        .expect("parse config overrides");

    assert_eq!(
        parsed_overrides,
        vec![
            (
                "model".to_string(),
                TomlValue::String("gpt-5-codex".to_string()),
            ),
            (
                "sandbox_mode".to_string(),
                TomlValue::String("read-only".to_string()),
            ),
        ]
    );
}

#[test]
fn app_server_accepts_process_scoped_grpc_code_mode_host() {
    let args = AppServerArgs::try_parse_from([
        "codex-app-server",
        "--code-mode-host",
        "https://example.test",
        "--listen",
        "off",
    ])
    .expect("parse gRPC app-server args");

    assert_eq!(
        args.code_mode_host.code_mode_host,
        Some(Url::parse("https://example.test").expect("test endpoint should parse"))
    );
    assert_eq!(args.listen, "off");
}

#[test]
fn managed_package_lane_selects_only_implicit_lane_socket() {
    for local_package_lane in [LocalPackageLane::Codex, LocalPackageLane::CdxDev] {
        let transport =
            transport_for_listen_url("unix://", Some(local_package_lane)).expect("lane listen URL");
        let codex_home = codex_core::config::find_codex_home().expect("codex home");
        assert_eq!(
            transport,
            AppServerTransport::UnixSocket {
                socket_path: codex_app_server_transport::app_server_control_socket_path_for_local_package_lane(
                    &codex_home,
                    local_package_lane,
                )
                .expect("lane socket")
            }
        );
    }

    for listen_url in [
        "stdio://",
        "unix:///tmp/codex.sock",
        "off",
        "ws://127.0.0.1:4545",
    ] {
        assert_eq!(
            transport_for_listen_url(listen_url, Some(LocalPackageLane::Codex))
                .expect("unchanged transport"),
            AppServerTransport::from_listen_url(listen_url).expect("generic transport")
        );
    }
}

#[test]
fn app_server_rejects_invalid_code_mode_host() {
    for endpoint in [
        "ftp://127.0.0.1:8765",
        "ws://",
        "ws://127.0.0.1:8765",
        "wss://example.test/code-mode",
        "ws://alice:secret@example.test/code-mode",
        "wss://alice:secret@example.test/code-mode",
        "wss://example.test/code-mode#fragment",
        "http://",
        "https://example.test/#fragment",
        "https://example.test/code-mode",
        "http://alice:secret@example.test",
        "https://alice:secret@example.test",
        "http://example.test/?token=secret",
    ] {
        let error =
            AppServerArgs::try_parse_from(["codex-app-server", "--code-mode-host", endpoint])
                .expect_err("invalid code-mode host endpoint should fail startup argument parsing");

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        let rendered_error = error.to_string();
        assert!(!rendered_error.contains("alice"));
        assert!(!rendered_error.contains("secret"));
    }
}
