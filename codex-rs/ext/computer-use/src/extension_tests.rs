use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use codex_config::McpServerTransportConfig;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ExtensionWarning;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use pretty_assertions::assert_eq;

use crate::CODEX_COMPUTER_USE_MCP_BIN_ENV_VAR;
use crate::CODEX_COMPUTER_USE_SKY_BIN_ENV_VAR;
use crate::COMPUTER_USE_SERVER_NAME;
use crate::extension::RuntimeLocator;
use crate::extension::install_with_locator;
use crate::vendored_artifact_path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VENDORED_SKY_LINUX_X64: &str = "sky/0.6.2/bin/linux/sky_linux_x64";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn valid_override_pair_contributes_required_stdio_server() -> TestResult {
    let config = test_config().await?;
    let tempdir = tempfile::tempdir()?;
    let sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let mcp_bin = tempdir.path().join("codex-computer-use-mcp");
    copy_executable(&sky_bin, &mcp_bin)?;

    let contributions = contribute_global(
        &config,
        RuntimeLocator::new_for_test(Some(mcp_bin.clone()), Some(sky_bin.clone())),
    )
    .await;

    let [McpServerContribution::Set { name, config }] = contributions.as_slice() else {
        panic!("expected one computer_use registration");
    };
    assert_eq!(name, COMPUTER_USE_SERVER_NAME);
    let McpServerTransportConfig::Stdio {
        command,
        args,
        env,
        env_vars,
        cwd,
    } = &config.transport
    else {
        panic!("computer_use should use stdio transport");
    };
    assert_eq!(Path::new(command), mcp_bin.as_path());
    assert_eq!(
        args,
        &vec!["--sky-bin".to_string(), sky_bin.display().to_string()]
    );
    assert_eq!(env, &None);
    assert!(env_vars.is_empty());
    assert_eq!(cwd, &None);
    assert!(config.required);
    assert!(!config.supports_parallel_tool_calls);

    Ok(())
}

#[tokio::test]
async fn missing_source_runtime_pair_removes_server() -> TestResult {
    let config = test_config().await?;
    let contributions = contribute_global(&config, RuntimeLocator::new_for_test(None, None)).await;

    assert!(matches!(
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    Ok(())
}

#[tokio::test]
async fn incomplete_override_emits_one_warning_per_thread() -> TestResult {
    let config = test_config().await?;
    let sink = Arc::new(RecordingEventSink::default());
    let mut builder = ExtensionRegistryBuilder::with_event_sink(sink.clone());
    install_with_locator(
        &mut builder,
        RuntimeLocator::new_for_test(Some(PathBuf::from("/tmp/computer-use-mcp")), None),
    );
    let registry = builder.build();
    let contributor = registry
        .mcp_server_contributors()
        .first()
        .expect("computer use contributor should be installed");
    let thread_init = ExtensionDataInit::default();
    let thread_store = ExtensionData::new("thread-1");

    let first = contributor
        .contribute(McpServerContributionContext::for_step(
            &config,
            &thread_init,
            &thread_store,
            "test-originator",
            &[],
            None,
        ))
        .await;
    let second = contributor
        .contribute(McpServerContributionContext::for_step(
            &config,
            &thread_init,
            &thread_store,
            "test-originator",
            &[],
            None,
        ))
        .await;

    assert!(matches!(
        first.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    assert!(matches!(
        second.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));

    let warnings = sink.warnings();
    assert_eq!(warnings.len(), 1);
    let warning = &warnings[0];
    assert_eq!(warning.thread_id, "thread-1");
    assert_eq!(warning.turn_id, None);
    assert!(warning.message.starts_with("Computer Use is unavailable: "));
    assert!(warning.message.contains(CODEX_COMPUTER_USE_MCP_BIN_ENV_VAR));
    assert!(warning.message.contains(CODEX_COMPUTER_USE_SKY_BIN_ENV_VAR));
    assert!(
        warning
            .message
            .contains("source/development execution only")
    );

    Ok(())
}

async fn test_config() -> Result<Config, Box<dyn std::error::Error>> {
    let codex_home = tempfile::tempdir()?;
    Ok(ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .cli_overrides(vec![("features.computer_use".to_string(), true.into())])
        .build()
        .await?)
}

async fn contribute_global(
    config: &Config,
    runtime_locator: RuntimeLocator,
) -> Vec<McpServerContribution> {
    let mut builder = ExtensionRegistryBuilder::new();
    install_with_locator(&mut builder, runtime_locator);
    let registry = builder.build();
    let contributor = registry
        .mcp_server_contributors()
        .first()
        .expect("computer use contributor should be installed");
    contributor
        .contribute(McpServerContributionContext::global(config))
        .await
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn copy_executable(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::copy(source, destination)?;
    set_executable(destination)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)
}

#[derive(Default)]
struct RecordingEventSink {
    warnings: Mutex<Vec<ExtensionWarning>>,
}

impl RecordingEventSink {
    fn warnings(&self) -> Vec<ExtensionWarning> {
        self.warnings
            .lock()
            .expect("warning buffer should not be poisoned")
            .clone()
    }
}

impl ExtensionEventSink for RecordingEventSink {
    fn emit(&self, _event: codex_protocol::protocol::Event) {}

    fn emit_warning(&self, warning: ExtensionWarning) {
        self.warnings
            .lock()
            .expect("warning buffer should not be poisoned")
            .push(warning);
    }
}
