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

use crate::COMPUTER_USE_SERVER_NAME;
use crate::extension::PackageRuntimeResources;
use crate::extension::RuntimeLocator;
use crate::extension::RuntimePathOverrides;
use crate::extension::install_with_locator;
use crate::vendored_artifact_path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VENDORED_SKY_LINUX_X64: &str = "sky/0.6.2/bin/linux/sky_linux_x64";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn valid_override_pair_contributes_required_stdio_server() -> TestResult {
    let config = test_config().await?;
    let tempdir = tempfile::tempdir()?;
    let vendored_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let sky_bin = tempdir.path().join("sky");
    let mcp_bin = tempdir.path().join("codex-computer-use-mcp");
    copy_executable(&vendored_sky_bin, &mcp_bin)?;
    copy_executable(&vendored_sky_bin, &sky_bin)?;

    let contributions = contribute_global(
        &config,
        RuntimeLocator::new_for_test(RuntimePathOverrides {
            mcp_bin: Some(mcp_bin.clone()),
            sky_bin: Some(sky_bin.clone()),
            ..Default::default()
        }),
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

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn unbundled_config_pair_contributes_required_stdio_server_with_runtime_args() -> TestResult {
    let tempdir = tempfile::tempdir()?;
    let vendored_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let sky_bin = tempdir.path().join("sky");
    let mcp_bin = tempdir.path().join("codex-computer-use-mcp");
    copy_executable(&vendored_sky_bin, &mcp_bin)?;
    copy_executable(&vendored_sky_bin, &sky_bin)?;
    let config = test_config_with_contents(&format!(
        r#"[features.computer_use]
enabled = true
mcp_bin = "{mcp_bin}"
sky_bin = "{sky_bin}"
xvfb = "/usr/bin/Xvfb"
openbox = "/usr/bin/openbox"
temp_root = "/tmp/codex-computer-use"
display_ready_timeout = 7000
shutdown_grace_period = 3000
"#,
        mcp_bin = mcp_bin.display(),
        sky_bin = sky_bin.display(),
    ))
    .await?;

    let contributions = contribute_global(
        &config,
        RuntimeLocator::new_for_test(RuntimePathOverrides::default()),
    )
    .await;

    let [McpServerContribution::Set { name, config }] = contributions.as_slice() else {
        panic!("expected one computer_use registration");
    };
    assert_eq!(name, COMPUTER_USE_SERVER_NAME);
    let McpServerTransportConfig::Stdio { command, args, .. } = &config.transport else {
        panic!("computer_use should use stdio transport");
    };
    assert_eq!(Path::new(command), mcp_bin.as_path());
    assert_eq!(
        args,
        &vec![
            "--sky-bin".to_string(),
            sky_bin.display().to_string(),
            "--xvfb".to_string(),
            "/usr/bin/Xvfb".to_string(),
            "--openbox".to_string(),
            "/usr/bin/openbox".to_string(),
            "--temp-root".to_string(),
            "/tmp/codex-computer-use".to_string(),
            "--display-ready-timeout-ms".to_string(),
            "7000".to_string(),
            "--shutdown-grace-period-ms".to_string(),
            "3000".to_string(),
        ]
    );

    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn env_path_overrides_take_precedence_over_package_and_configured_paths() -> TestResult {
    let tempdir = tempfile::tempdir()?;
    let configured_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let configured_mcp_bin = tempdir.path().join("configured-codex-computer-use-mcp");
    let override_mcp_bin = tempdir.path().join("override-codex-computer-use-mcp");
    let override_sky_bin = tempdir.path().join("override-sky");
    copy_executable(&configured_sky_bin, &configured_mcp_bin)?;
    copy_executable(&configured_sky_bin, &override_mcp_bin)?;
    copy_executable(&configured_sky_bin, &override_sky_bin)?;
    let package_resources = package_runtime_resources(tempdir.path())?;
    let config = test_config_with_contents(&format!(
        r#"[features.computer_use]
enabled = true
mcp_bin = "{configured_mcp_bin}"
sky_bin = "{configured_sky_bin}"
xvfb = "/usr/bin/Xvfb-config"
openbox = "/usr/bin/openbox-config"
temp_root = "/tmp/codex-computer-use-config"
display_ready_timeout = 7000
shutdown_grace_period = 3000
"#,
        configured_mcp_bin = configured_mcp_bin.display(),
        configured_sky_bin = configured_sky_bin.display(),
    ))
    .await?;

    let contributions = contribute_global(
        &config,
        RuntimeLocator::new_for_test_with_package_resources(
            RuntimePathOverrides {
                mcp_bin: Some(override_mcp_bin.clone()),
                sky_bin: Some(override_sky_bin.clone()),
                xvfb: Some(PathBuf::from("/usr/bin/Xvfb-override")),
                openbox: Some(PathBuf::from("/usr/bin/openbox-override")),
                temp_root: Some(PathBuf::from("/tmp/codex-computer-use-override")),
            },
            Some(package_resources),
        ),
    )
    .await;

    let [McpServerContribution::Set { name, config }] = contributions.as_slice() else {
        panic!("expected one computer_use registration");
    };
    assert_eq!(name, COMPUTER_USE_SERVER_NAME);
    let McpServerTransportConfig::Stdio { command, args, .. } = &config.transport else {
        panic!("computer_use should use stdio transport");
    };
    assert_eq!(Path::new(command), override_mcp_bin.as_path());
    assert_eq!(
        args,
        &vec![
            "--sky-bin".to_string(),
            override_sky_bin.display().to_string(),
            "--xvfb".to_string(),
            "/usr/bin/Xvfb-override".to_string(),
            "--openbox".to_string(),
            "/usr/bin/openbox-override".to_string(),
            "--temp-root".to_string(),
            "/tmp/codex-computer-use-override".to_string(),
            "--display-ready-timeout-ms".to_string(),
            "7000".to_string(),
            "--shutdown-grace-period-ms".to_string(),
            "3000".to_string(),
        ]
    );

    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn package_pair_takes_precedence_over_configured_paths() -> TestResult {
    let tempdir = tempfile::tempdir()?;
    let configured_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let configured_mcp_bin = tempdir.path().join("configured-codex-computer-use-mcp");
    copy_executable(&configured_sky_bin, &configured_mcp_bin)?;
    let package_resources = package_runtime_resources(tempdir.path())?;
    let package_mcp_bin = package_resources
        .mcp_bin
        .clone()
        .expect("package MCP resource should exist");
    let package_sky_bin = package_resources
        .sky_bin
        .clone()
        .expect("package Sky resource should exist");
    let config = test_config_with_contents(&format!(
        r#"[features.computer_use]
enabled = true
mcp_bin = "{configured_mcp_bin}"
sky_bin = "{configured_sky_bin}"
"#,
        configured_mcp_bin = configured_mcp_bin.display(),
        configured_sky_bin = configured_sky_bin.display(),
    ))
    .await?;

    let contributions = contribute_global(
        &config,
        RuntimeLocator::new_for_test_with_package_resources(
            RuntimePathOverrides::default(),
            Some(package_resources),
        ),
    )
    .await;

    let [McpServerContribution::Set { config, .. }] = contributions.as_slice() else {
        panic!("expected one computer_use registration");
    };
    let McpServerTransportConfig::Stdio { command, args, .. } = &config.transport else {
        panic!("computer_use should use stdio transport");
    };
    assert_eq!(Path::new(command), package_mcp_bin.as_path());
    assert_eq!(
        args,
        &vec![
            "--sky-bin".to_string(),
            package_sky_bin.display().to_string()
        ]
    );

    Ok(())
}

#[tokio::test]
async fn missing_source_runtime_pair_removes_server() -> TestResult {
    let config = test_config().await?;
    let contributions = contribute_global(
        &config,
        RuntimeLocator::new_for_test(RuntimePathOverrides::default()),
    )
    .await;

    assert!(matches!(
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    Ok(())
}

#[tokio::test]
async fn incomplete_environment_pair_emits_one_warning_per_thread() -> TestResult {
    let config = test_config().await?;
    let sink = Arc::new(RecordingEventSink::default());
    let mut builder = ExtensionRegistryBuilder::with_event_sink(sink.clone());
    install_with_locator(
        &mut builder,
        RuntimeLocator::new_for_test(RuntimePathOverrides {
            mcp_bin: Some(PathBuf::from("/tmp/computer-use-mcp")),
            ..Default::default()
        }),
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
    assert!(warning.message.contains("[features.computer_use]"));
    assert!(warning.message.contains("package resources"));
    assert!(warning.message.contains("CODEX_COMPUTER_USE_*"));
    assert!(
        !warning
            .message
            .contains("source/development execution only")
    );

    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn incomplete_package_pair_does_not_fall_back_to_configured_paths() -> TestResult {
    let config = test_config_with_contents(
        r#"[features.computer_use]
enabled = true
mcp_bin = "/tmp/configured-computer-use-mcp"
sky_bin = "/tmp/configured-sky"
"#,
    )
    .await?;
    let sink = Arc::new(RecordingEventSink::default());
    let contributions = contribute_for_thread(
        &config,
        RuntimeLocator::new_for_test_with_package_resources(
            RuntimePathOverrides::default(),
            Some(PackageRuntimeResources {
                mcp_bin: Some(PathBuf::from("/tmp/package-computer-use-mcp")),
                sky_bin: None,
            }),
        ),
        sink.clone(),
        "thread-incomplete-package",
    )
    .await;

    assert!(matches!(
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    let warnings = sink.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .message
            .contains("package runtime requires both codex-computer-use-mcp and sky_linux_x64")
    );

    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn missing_package_pair_does_not_fall_back_to_valid_configured_paths() -> TestResult {
    let tempdir = tempfile::tempdir()?;
    let configured_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let configured_mcp_bin = tempdir.path().join("configured-codex-computer-use-mcp");
    copy_executable(&configured_sky_bin, &configured_mcp_bin)?;
    let config = test_config_with_contents(&format!(
        r#"[features.computer_use]
enabled = true
mcp_bin = "{configured_mcp_bin}"
sky_bin = "{configured_sky_bin}"
"#,
        configured_mcp_bin = configured_mcp_bin.display(),
        configured_sky_bin = configured_sky_bin.display(),
    ))
    .await?;
    let sink = Arc::new(RecordingEventSink::default());
    let contributions = contribute_for_thread(
        &config,
        RuntimeLocator::new_for_test_with_package_resources(
            RuntimePathOverrides::default(),
            Some(PackageRuntimeResources::default()),
        ),
        sink.clone(),
        "thread-missing-package",
    )
    .await;

    assert!(matches!(
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    let warnings = sink.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .message
            .contains("package runtime requires both codex-computer-use-mcp and sky_linux_x64")
    );

    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn incomplete_config_pair_removes_server() -> TestResult {
    let config = test_config_with_contents(
        r#"[features.computer_use]
enabled = true
mcp_bin = "/tmp/configured-computer-use-mcp"
"#,
    )
    .await?;
    let sink = Arc::new(RecordingEventSink::default());
    let contributions = contribute_for_thread(
        &config,
        RuntimeLocator::new_for_test(RuntimePathOverrides::default()),
        sink.clone(),
        "thread-incomplete-config",
    )
    .await;

    assert!(matches!(
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    let warnings = sink.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .message
            .contains("requires both mcp_bin and sky_bin when either path is configured")
    );

    Ok(())
}

#[tokio::test]
async fn disabled_feature_does_not_resolve_incomplete_runtime_sources() -> TestResult {
    let config = test_config_with_contents("features.computer_use = false\n").await?;
    let sink = Arc::new(RecordingEventSink::default());
    let contributions = contribute_for_thread(
        &config,
        RuntimeLocator::new_for_test_with_package_resources(
            RuntimePathOverrides {
                mcp_bin: Some(PathBuf::from("/tmp/override-computer-use-mcp")),
                ..Default::default()
            },
            Some(PackageRuntimeResources {
                mcp_bin: Some(PathBuf::from("/tmp/package-computer-use-mcp")),
                sky_bin: None,
            }),
        ),
        sink.clone(),
        "thread-disabled",
    )
    .await;

    assert!(matches!(
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));
    assert!(sink.warnings().is_empty());

    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn invalid_xvfb_override_emits_generic_warning() -> TestResult {
    let tempdir = tempfile::tempdir()?;
    let vendored_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);
    let sky_bin = tempdir.path().join("sky");
    let mcp_bin = tempdir.path().join("codex-computer-use-mcp");
    copy_executable(&vendored_sky_bin, &mcp_bin)?;
    copy_executable(&vendored_sky_bin, &sky_bin)?;
    let config = test_config_with_contents(&format!(
        r#"[features.computer_use]
enabled = true
mcp_bin = "{mcp_bin}"
sky_bin = "{sky_bin}"
"#,
        mcp_bin = mcp_bin.display(),
        sky_bin = sky_bin.display(),
    ))
    .await?;
    let sink = Arc::new(RecordingEventSink::default());
    let mut builder = ExtensionRegistryBuilder::with_event_sink(sink.clone());
    install_with_locator(
        &mut builder,
        RuntimeLocator::new_for_test(RuntimePathOverrides {
            xvfb: Some(PathBuf::new()),
            ..Default::default()
        }),
    );
    let registry = builder.build();
    let contributor = registry
        .mcp_server_contributors()
        .first()
        .expect("computer use contributor should be installed");
    let thread_init = ExtensionDataInit::default();
    let thread_store = ExtensionData::new("thread-xvfb");

    let contributions = contributor
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
        contributions.as_slice(),
        [McpServerContribution::Remove { name }] if name == COMPUTER_USE_SERVER_NAME
    ));

    let warnings = sink.warnings();
    assert_eq!(warnings.len(), 1);
    let warning = &warnings[0];
    assert_eq!(warning.thread_id, "thread-xvfb");
    assert_eq!(warning.turn_id, None);
    assert!(warning.message.starts_with("Computer Use is unavailable: "));
    assert!(warning.message.contains("invalid Computer Use xvfb"));
    assert!(warning.message.contains("[features.computer_use]"));
    assert!(warning.message.contains("package resources"));
    assert!(warning.message.contains("CODEX_COMPUTER_USE_*"));
    assert!(
        !warning
            .message
            .contains("source/development execution only")
    );
    assert!(!warning.message.contains(".mcp_bin/.sky_bin"));

    Ok(())
}

async fn test_config() -> Result<Config, Box<dyn std::error::Error>> {
    test_config_with_contents("features.computer_use = true\n").await
}

async fn test_config_with_contents(contents: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let codex_home = tempfile::tempdir()?;
    std::fs::write(
        codex_home.path().join(codex_config::CONFIG_TOML_FILE),
        contents,
    )?;
    Ok(ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
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

async fn contribute_for_thread(
    config: &Config,
    runtime_locator: RuntimeLocator,
    sink: Arc<RecordingEventSink>,
    thread_id: &str,
) -> Vec<McpServerContribution> {
    let mut builder = ExtensionRegistryBuilder::with_event_sink(sink);
    install_with_locator(&mut builder, runtime_locator);
    let registry = builder.build();
    let contributor = registry
        .mcp_server_contributors()
        .first()
        .expect("computer use contributor should be installed");
    let thread_init = ExtensionDataInit::default();
    let thread_store = ExtensionData::new(thread_id);

    contributor
        .contribute(McpServerContributionContext::for_step(
            config,
            &thread_init,
            &thread_store,
            "test-originator",
            &[],
            None,
        ))
        .await
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn copy_executable(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::copy(source, destination)?;
    set_executable(destination)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn package_runtime_resources(root: &Path) -> io::Result<PackageRuntimeResources> {
    let resources_dir = root.join("codex-resources");
    let mcp_bin = resources_dir.join("codex-computer-use-mcp");
    let sky_bin = resources_dir.join("sky_linux_x64");
    let vendored_sky_bin = vendored_artifact_path(VENDORED_SKY_LINUX_X64);

    std::fs::create_dir_all(resources_dir)?;
    copy_executable(&vendored_sky_bin, &mcp_bin)?;
    copy_executable(&vendored_sky_bin, &sky_bin)?;

    Ok(PackageRuntimeResources {
        mcp_bin: Some(mcp_bin),
        sky_bin: Some(sky_bin),
    })
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
