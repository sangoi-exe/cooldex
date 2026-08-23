use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID;
use codex_config::McpServerConfig;
use codex_config::McpServerTransportConfig;
use codex_core::config::Config;
use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ExtensionWarning;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_features::Feature;

use crate::COMPUTER_USE_SERVER_NAME;

pub const CODEX_COMPUTER_USE_MCP_BIN_ENV_VAR: &str = "CODEX_COMPUTER_USE_MCP_BIN";
pub const CODEX_COMPUTER_USE_SKY_BIN_ENV_VAR: &str = "CODEX_COMPUTER_USE_SKY_BIN";
pub const CODEX_COMPUTER_USE_XVFB_BIN_ENV_VAR: &str = "CODEX_COMPUTER_USE_XVFB_BIN";
pub const CODEX_COMPUTER_USE_OPENBOX_BIN_ENV_VAR: &str = "CODEX_COMPUTER_USE_OPENBOX_BIN";
pub const CODEX_COMPUTER_USE_TEMP_ROOT_ENV_VAR: &str = "CODEX_COMPUTER_USE_TEMP_ROOT";

const SUPPORTED_PLATFORM_REASON: &str =
    "unsupported platform; Computer Use currently requires Linux x86_64";
const MISSING_SOURCE_RUNTIME_PAIR_REASON: &str =
    "source/development runtime pair is not configured";
const INCOMPLETE_RUNTIME_PAIR_REASON: &str =
    "Computer Use requires both mcp_bin and sky_bin when either path is configured";
const INVALID_OVERRIDE_MCP_REASON: &str = "invalid Computer Use mcp_bin";
const INVALID_OVERRIDE_SKY_REASON: &str = "invalid Computer Use sky_bin";
const INVALID_OVERRIDE_XVFB_REASON: &str = "invalid Computer Use xvfb";
const INVALID_OVERRIDE_OPENBOX_REASON: &str = "invalid Computer Use openbox";
const INVALID_OVERRIDE_TEMP_ROOT_REASON: &str = "invalid Computer Use temp_root";
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LSB: u8 = 1;
const ELF_MACHINE_X86_64: u16 = 62;

#[derive(Clone)]
struct ComputerUseExtension {
    event_sink: Arc<dyn ExtensionEventSink>,
    runtime_locator: RuntimeLocator,
}

#[derive(Clone)]
pub(crate) struct RuntimeLocator {
    path_overrides: RuntimePathOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRuntimeConfig {
    mcp_bin: PathBuf,
    sky_bin: PathBuf,
    xvfb: Option<PathBuf>,
    openbox: Option<PathBuf>,
    temp_root: Option<PathBuf>,
    display_ready_timeout_ms: Option<u64>,
    shutdown_grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimePathOverrides {
    pub(crate) mcp_bin: Option<PathBuf>,
    pub(crate) sky_bin: Option<PathBuf>,
    pub(crate) xvfb: Option<PathBuf>,
    pub(crate) openbox: Option<PathBuf>,
    pub(crate) temp_root: Option<PathBuf>,
}

#[derive(Default)]
struct ComputerUseWarningState {
    emitted: AtomicBool,
}

impl RuntimeLocator {
    fn from_process() -> Self {
        Self {
            path_overrides: RuntimePathOverrides {
                mcp_bin: std::env::var_os(CODEX_COMPUTER_USE_MCP_BIN_ENV_VAR).map(PathBuf::from),
                sky_bin: std::env::var_os(CODEX_COMPUTER_USE_SKY_BIN_ENV_VAR).map(PathBuf::from),
                xvfb: std::env::var_os(CODEX_COMPUTER_USE_XVFB_BIN_ENV_VAR).map(PathBuf::from),
                openbox: std::env::var_os(CODEX_COMPUTER_USE_OPENBOX_BIN_ENV_VAR)
                    .map(PathBuf::from),
                temp_root: std::env::var_os(CODEX_COMPUTER_USE_TEMP_ROOT_ENV_VAR)
                    .map(PathBuf::from),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(path_overrides: RuntimePathOverrides) -> Self {
        Self { path_overrides }
    }

    fn resolve(&self, config: &Config) -> Result<ResolvedRuntimeConfig, &'static str> {
        if !computer_use_supported_platform() {
            return Err(SUPPORTED_PLATFORM_REASON);
        }

        let configured = &config.computer_use;
        let mcp_bin = self
            .path_overrides
            .mcp_bin
            .as_deref()
            .or(configured.mcp_bin.as_deref());
        let sky_bin = self
            .path_overrides
            .sky_bin
            .as_deref()
            .or(configured.sky_bin.as_deref());

        match (mcp_bin, sky_bin) {
            (Some(_), None) | (None, Some(_)) => Err(INCOMPLETE_RUNTIME_PAIR_REASON),
            (Some(mcp_bin), Some(sky_bin)) => Ok(ResolvedRuntimeConfig {
                mcp_bin: validate_linux_x64_binary(mcp_bin, INVALID_OVERRIDE_MCP_REASON)?,
                sky_bin: validate_linux_x64_binary(sky_bin, INVALID_OVERRIDE_SKY_REASON)?,
                xvfb: validate_nonempty_path(
                    self.path_overrides
                        .xvfb
                        .as_deref()
                        .or(configured.xvfb.as_deref()),
                    INVALID_OVERRIDE_XVFB_REASON,
                )?,
                openbox: validate_nonempty_path(
                    self.path_overrides
                        .openbox
                        .as_deref()
                        .or(configured.openbox.as_deref()),
                    INVALID_OVERRIDE_OPENBOX_REASON,
                )?,
                temp_root: validate_nonempty_path(
                    self.path_overrides
                        .temp_root
                        .as_deref()
                        .or(configured.temp_root.as_deref()),
                    INVALID_OVERRIDE_TEMP_ROOT_REASON,
                )?,
                display_ready_timeout_ms: configured.display_ready_timeout_ms,
                shutdown_grace_period_ms: configured.shutdown_grace_period_ms,
            }),
            (None, None) => Err(MISSING_SOURCE_RUNTIME_PAIR_REASON),
        }
    }
}

impl ComputerUseExtension {
    fn new(event_sink: Arc<dyn ExtensionEventSink>, runtime_locator: RuntimeLocator) -> Self {
        Self {
            event_sink,
            runtime_locator,
        }
    }

    fn emit_unavailable_warning(
        &self,
        context: McpServerContributionContext<'_, Config>,
        reason: &'static str,
    ) {
        let message = format!(
            "Computer Use is unavailable: {reason}. Check [features.computer_use] and CODEX_COMPUTER_USE_* overrides; source/development execution only."
        );
        let Some(thread_store) = context.thread_store() else {
            tracing::warn!(%message, "computer use MCP server is unavailable");
            return;
        };

        let warning_state = thread_store.get_or_init(ComputerUseWarningState::default);
        let first_emit = warning_state
            .emitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !first_emit {
            return;
        }

        self.event_sink.emit_warning(ExtensionWarning {
            thread_id: thread_store.level_id().to_string(),
            turn_id: None,
            message,
        });
    }
}

impl McpServerContributor<Config> for ComputerUseExtension {
    fn id(&self) -> &'static str {
        "computer_use"
    }

    fn contribute<'a>(
        &'a self,
        context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move {
            let remove = || {
                vec![McpServerContribution::Remove {
                    name: COMPUTER_USE_SERVER_NAME.to_string(),
                }]
            };

            if !context.config().features.enabled(Feature::ComputerUse) {
                return remove();
            }

            let resolved = match self.runtime_locator.resolve(context.config()) {
                Ok(resolved) => resolved,
                Err(reason) => {
                    self.emit_unavailable_warning(context, reason);
                    return remove();
                }
            };

            vec![McpServerContribution::Set {
                name: COMPUTER_USE_SERVER_NAME.to_string(),
                config: Box::new(stdio_server_config(&resolved)),
            }]
        })
    }
}

pub fn install(builder: &mut ExtensionRegistryBuilder<Config>) {
    install_with_locator(builder, RuntimeLocator::from_process());
}

pub(crate) fn install_with_locator(
    builder: &mut ExtensionRegistryBuilder<Config>,
    runtime_locator: RuntimeLocator,
) {
    builder.mcp_server_contributor(Arc::new(ComputerUseExtension::new(
        builder.event_sink(),
        runtime_locator,
    )));
}

fn stdio_server_config(paths: &ResolvedRuntimeConfig) -> McpServerConfig {
    let mut args = vec!["--sky-bin".to_string(), paths.sky_bin.display().to_string()];
    push_path_arg(&mut args, "--xvfb", paths.xvfb.as_ref());
    push_path_arg(&mut args, "--openbox", paths.openbox.as_ref());
    push_path_arg(&mut args, "--temp-root", paths.temp_root.as_ref());
    push_u64_arg(
        &mut args,
        "--display-ready-timeout-ms",
        paths.display_ready_timeout_ms,
    );
    push_u64_arg(
        &mut args,
        "--shutdown-grace-period-ms",
        paths.shutdown_grace_period_ms,
    );

    McpServerConfig {
        transport: McpServerTransportConfig::Stdio {
            command: paths.mcp_bin.display().to_string(),
            args,
            env: None,
            env_vars: Vec::new(),
            cwd: None,
        },
        auth: Default::default(),
        environment_id: DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
        enabled: true,
        required: true,
        supports_parallel_tool_calls: false,
        omit_tools_from: None,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        default_tools_approval_mode: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth: None,
        oauth_resource: None,
        tools: HashMap::new(),
    }
}

fn validate_linux_x64_binary(
    path: &Path,
    invalid_reason: &'static str,
) -> Result<PathBuf, &'static str> {
    if !path.is_absolute() {
        return Err(invalid_reason);
    }

    let metadata = fs::metadata(path).map_err(|_| invalid_reason)?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(invalid_reason);
    }
    if !is_linux_x64_elf(path).map_err(|_| invalid_reason)? {
        return Err(invalid_reason);
    }

    Ok(path.to_path_buf())
}

fn validate_nonempty_path(
    path: Option<&Path>,
    invalid_reason: &'static str,
) -> Result<Option<PathBuf>, &'static str> {
    match path {
        Some(path) if path.as_os_str().is_empty() => Err(invalid_reason),
        Some(path) => Ok(Some(path.to_path_buf())),
        None => Ok(None),
    }
}

fn push_path_arg(args: &mut Vec<String>, flag: &str, path: Option<&PathBuf>) {
    if let Some(path) = path {
        args.push(flag.to_string());
        args.push(path.display().to_string());
    }
}

fn push_u64_arg(args: &mut Vec<String>, flag: &str, value: Option<u64>) {
    if let Some(value) = value {
        args.push(flag.to_string());
        args.push(value.to_string());
    }
}

fn is_linux_x64_elf(path: &Path) -> io::Result<bool> {
    let mut header = [0_u8; 20];
    let mut file = File::open(path)?;
    if let Err(error) = file.read_exact(&mut header) {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            Ok(false)
        } else {
            Err(error)
        };
    }
    if &header[..4] != b"\x7FELF" {
        return Ok(false);
    }
    if header[4] != ELF_CLASS_64 || header[5] != ELF_DATA_LSB {
        return Ok(false);
    }

    let e_machine = u16::from_le_bytes([header[18], header[19]]);
    Ok(e_machine == ELF_MACHINE_X86_64)
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn computer_use_supported_platform() -> bool {
    cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")
}
