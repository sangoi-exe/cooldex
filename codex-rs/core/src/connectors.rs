use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use async_channel::unbounded;
pub use codex_app_server_protocol::AppBranding;
pub use codex_app_server_protocol::AppInfo;
pub use codex_app_server_protocol::AppMetadata;
use codex_connectors::AllConnectorsCacheKey;
use codex_connectors::DirectoryListResponse;
use codex_protocol::protocol::SandboxPolicy;
use codex_tools::DiscoverableTool;
use rmcp::model::ToolAnnotations;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tracing::warn;

use crate::config::Config;
use crate::config_loader::AppsRequirementsToml;
use crate::mcp::McpManager;
use crate::plugins::PluginsManager;
use crate::plugins::list_tool_suggest_discoverable_plugins;
use crate::session::INITIAL_SUBMIT_ID;
use codex_config::types::AppToolApproval;
use codex_config::types::AppsConfigToml;
use codex_config::types::ToolSuggestDiscoverableType;
use codex_features::Feature;
use codex_login::ChatGptRequestAuth;
use codex_login::default_client::create_client;
use codex_login::default_client::originator;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_mcp::McpConnectionManager;
use codex_mcp::McpRuntimeEnvironment;
use codex_mcp::ToolInfo;
use codex_mcp::ToolPluginProvenance;
use codex_mcp::codex_apps_tools_cache_key;
use codex_mcp::compute_auth_statuses;
use codex_mcp::with_codex_apps_mcp;
use reqwest::header::CONTENT_TYPE;

const CONNECTORS_READY_TIMEOUT_ON_EMPTY_TOOLS: Duration = Duration::from_secs(30);
const DIRECTORY_CONNECTORS_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTOR_HTTP_ERROR_PREVIEW_LIMIT: usize = 160;
const CONNECTOR_HTML_ERROR_BODY_OMITTED: &str = "response body omitted for text/html";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppToolPolicy {
    pub enabled: bool,
    pub approval: AppToolApproval,
}

impl Default for AppToolPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            approval: AppToolApproval::Auto,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AccessibleConnectorsCacheKey {
    chatgpt_base_url: String,
    account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    is_workspace_account: bool,
}

#[derive(Clone)]
struct CachedAccessibleConnectors {
    key: AccessibleConnectorsCacheKey,
    expires_at: Instant,
    connectors: Vec<AppInfo>,
}

static ACCESSIBLE_CONNECTORS_CACHE: LazyLock<StdMutex<Option<CachedAccessibleConnectors>>> =
    LazyLock::new(|| StdMutex::new(None));

#[derive(Debug, Clone)]
pub struct AccessibleConnectorsStatus {
    pub connectors: Vec<AppInfo>,
    pub codex_apps_ready: bool,
}

pub async fn list_accessible_connectors_from_mcp_tools(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(
        list_accessible_connectors_from_mcp_tools_with_options_and_status(
            config, auth, /*force_refetch*/ false,
        )
        .await?
        .connectors,
    )
}

pub(crate) async fn list_accessible_and_enabled_connectors_from_manager(
    mcp_connection_manager: &McpConnectionManager,
    config: &Config,
) -> Vec<AppInfo> {
    with_app_enabled_state(
        accessible_connectors_from_mcp_tools(&mcp_connection_manager.list_all_tools().await),
        config,
    )
    .into_iter()
    .filter(|connector| connector.is_accessible && connector.is_enabled)
    .collect()
}

pub(crate) async fn list_tool_suggest_discoverable_tools_with_auth(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
    accessible_connectors: &[AppInfo],
) -> anyhow::Result<Vec<DiscoverableTool>> {
    let directory_connectors =
        list_directory_connectors_for_tool_suggest_with_auth(config, auth).await?;
    let connector_ids = tool_suggest_connector_ids(config).await;
    let discoverable_connectors =
        codex_connectors::filter::filter_tool_suggest_discoverable_connectors(
            directory_connectors,
            accessible_connectors,
            &connector_ids,
            originator().value.as_str(),
        )
        .into_iter()
        .map(DiscoverableTool::from);
    let discoverable_plugins = list_tool_suggest_discoverable_plugins(config)
        .await?
        .into_iter()
        .map(DiscoverableTool::from);
    Ok(discoverable_connectors
        .chain(discoverable_plugins)
        .collect())
}

pub async fn list_cached_accessible_connectors_from_mcp_tools(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
) -> Option<Vec<AppInfo>> {
    if !config.features.apps_enabled_for_auth(auth.is_some()) {
        return Some(Vec::new());
    }
    let cache_key = accessible_connectors_cache_key(config, auth);
    read_cached_accessible_connectors(&cache_key).map(|connectors| {
        codex_connectors::filter::filter_disallowed_connectors(
            connectors,
            originator().value.as_str(),
        )
    })
}

pub(crate) fn refresh_accessible_connectors_cache_from_mcp_tools(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
    mcp_tools: &HashMap<String, ToolInfo>,
) {
    if !config.features.enabled(Feature::Apps) {
        return;
    }

    let cache_key = accessible_connectors_cache_key(config, auth);
    let accessible_connectors = codex_connectors::filter::filter_disallowed_connectors(
        accessible_connectors_from_mcp_tools(mcp_tools),
        originator().value.as_str(),
    );
    write_cached_accessible_connectors(cache_key, &accessible_connectors);
}

pub async fn list_accessible_connectors_from_mcp_tools_with_options(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
    force_refetch: bool,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(
        list_accessible_connectors_from_mcp_tools_with_options_and_status(
            config,
            auth,
            force_refetch,
        )
        .await?
        .connectors,
    )
}

pub async fn list_accessible_connectors_from_mcp_tools_with_options_and_status(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
    force_refetch: bool,
) -> anyhow::Result<AccessibleConnectorsStatus> {
    if !config.features.apps_enabled_for_auth(auth.is_some()) {
        return Ok(AccessibleConnectorsStatus {
            connectors: Vec::new(),
            codex_apps_ready: true,
        });
    }
    let cache_key = accessible_connectors_cache_key(config, auth);
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.to_path_buf()));
    let mcp_manager = McpManager::new(Arc::clone(&plugins_manager));
    let tool_plugin_provenance = mcp_manager.tool_plugin_provenance(config).await;
    if !force_refetch && let Some(cached_connectors) = read_cached_accessible_connectors(&cache_key)
    {
        let cached_connectors = codex_connectors::filter::filter_disallowed_connectors(
            cached_connectors,
            originator().value.as_str(),
        );
        let cached_connectors = with_app_plugin_sources(cached_connectors, &tool_plugin_provenance);
        return Ok(AccessibleConnectorsStatus {
            connectors: cached_connectors,
            codex_apps_ready: true,
        });
    }

    let mcp_config = config.to_mcp_config(plugins_manager.as_ref()).await;
    let mcp_servers = with_codex_apps_mcp(HashMap::new(), auth, &mcp_config);
    if mcp_servers.is_empty() {
        return Ok(AccessibleConnectorsStatus {
            connectors: Vec::new(),
            codex_apps_ready: true,
        });
    }

    let auth_status_entries =
        compute_auth_statuses(mcp_servers.iter(), config.mcp_oauth_credentials_store_mode).await;

    let (tx_event, rx_event) = unbounded();
    drop(rx_event);

    let (mcp_connection_manager, cancel_token) = McpConnectionManager::new(
        &mcp_servers,
        config.mcp_oauth_credentials_store_mode,
        auth_status_entries,
        &config.permissions.approval_policy,
        INITIAL_SUBMIT_ID.to_owned(),
        tx_event,
        SandboxPolicy::new_read_only_policy(),
        McpRuntimeEnvironment::new(
            Arc::new(codex_exec_server::Environment::default()),
            config.cwd.to_path_buf(),
        ),
        config.codex_home.to_path_buf(),
        codex_apps_tools_cache_key(auth),
        ToolPluginProvenance::default(),
    )
    .await;

    let refreshed_tools = if force_refetch {
        match mcp_connection_manager
            .hard_refresh_codex_apps_tools_cache()
            .await
        {
            Ok(tools) => Some(tools),
            Err(err) => {
                warn!(
                    "failed to force-refresh tools for MCP server '{CODEX_APPS_MCP_SERVER_NAME}', using cached/startup tools: {err:#}"
                );
                None
            }
        }
    } else {
        None
    };
    let refreshed_tools_succeeded = refreshed_tools.is_some();

    let mut tools = if let Some(tools) = refreshed_tools {
        tools
    } else {
        mcp_connection_manager.list_all_tools().await
    };
    let mut should_reload_tools = false;
    let codex_apps_ready = if refreshed_tools_succeeded {
        true
    } else if let Some(cfg) = mcp_servers.get(CODEX_APPS_MCP_SERVER_NAME) {
        let immediate_ready = mcp_connection_manager
            .wait_for_server_ready(CODEX_APPS_MCP_SERVER_NAME, Duration::ZERO)
            .await;
        if immediate_ready {
            true
        } else if tools.is_empty() {
            let timeout = cfg
                .startup_timeout_sec
                .unwrap_or(CONNECTORS_READY_TIMEOUT_ON_EMPTY_TOOLS);
            let ready = mcp_connection_manager
                .wait_for_server_ready(CODEX_APPS_MCP_SERVER_NAME, timeout)
                .await;
            should_reload_tools = ready;
            ready
        } else {
            false
        }
    } else {
        false
    };
    if should_reload_tools {
        tools = mcp_connection_manager.list_all_tools().await;
    }
    if codex_apps_ready {
        cancel_token.cancel();
    }

    let accessible_connectors = codex_connectors::filter::filter_disallowed_connectors(
        accessible_connectors_from_mcp_tools(&tools),
        originator().value.as_str(),
    );
    if codex_apps_ready || !accessible_connectors.is_empty() {
        write_cached_accessible_connectors(cache_key, &accessible_connectors);
    }
    let accessible_connectors =
        with_app_plugin_sources(accessible_connectors, &tool_plugin_provenance);
    Ok(AccessibleConnectorsStatus {
        connectors: accessible_connectors,
        codex_apps_ready,
    })
}

fn accessible_connectors_cache_key(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
) -> AccessibleConnectorsCacheKey {
    AccessibleConnectorsCacheKey {
        chatgpt_base_url: config.chatgpt_base_url.clone(),
        account_id: auth.map(|auth| auth.account_id().to_string()),
        chatgpt_user_id: auth
            .and_then(ChatGptRequestAuth::chatgpt_user_id)
            .map(str::to_string),
        is_workspace_account: auth.is_some_and(ChatGptRequestAuth::is_workspace_account),
    }
}

fn read_cached_accessible_connectors(
    cache_key: &AccessibleConnectorsCacheKey,
) -> Option<Vec<AppInfo>> {
    let mut cache_guard = ACCESSIBLE_CONNECTORS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();

    if let Some(cached) = cache_guard.as_ref() {
        if now < cached.expires_at && cached.key == *cache_key {
            return Some(cached.connectors.clone());
        }
        if now >= cached.expires_at {
            *cache_guard = None;
        }
    }

    None
}

fn write_cached_accessible_connectors(
    cache_key: AccessibleConnectorsCacheKey,
    connectors: &[AppInfo],
) {
    let mut cache_guard = ACCESSIBLE_CONNECTORS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache_guard = Some(CachedAccessibleConnectors {
        key: cache_key,
        expires_at: Instant::now() + codex_connectors::CONNECTORS_CACHE_TTL,
        connectors: connectors.to_vec(),
    });
}

async fn tool_suggest_connector_ids(config: &Config) -> HashSet<String> {
    let mut connector_ids = PluginsManager::new(config.codex_home.to_path_buf())
        .plugins_for_config(config)
        .await
        .capability_summaries()
        .iter()
        .flat_map(|plugin| plugin.app_connector_ids.iter())
        .map(|connector_id| connector_id.0.clone())
        .collect::<HashSet<_>>();
    connector_ids.extend(
        config
            .tool_suggest
            .discoverables
            .iter()
            .filter(|discoverable| discoverable.kind == ToolSuggestDiscoverableType::Connector)
            .map(|discoverable| discoverable.id.clone()),
    );
    connector_ids
}

async fn list_directory_connectors_for_tool_suggest_with_auth(
    config: &Config,
    auth: Option<&ChatGptRequestAuth>,
) -> anyhow::Result<Vec<AppInfo>> {
    if !config.features.enabled(Feature::Apps) {
        return Ok(Vec::new());
    }

    let Some(request_auth) = auth else {
        return Ok(Vec::new());
    };

    let cache_key = AllConnectorsCacheKey::new(
        config.chatgpt_base_url.clone(),
        Some(request_auth.account_id().to_string()),
        request_auth.chatgpt_user_id().map(str::to_string),
        request_auth.is_workspace_account(),
    );

    codex_connectors::list_all_connectors_with_options(
        cache_key,
        request_auth.is_workspace_account(),
        /*force_refetch*/ false,
        |path| {
            let request_auth = request_auth.clone();
            async move {
                chatgpt_get_request_with_auth::<DirectoryListResponse>(config, path, request_auth)
                    .await
            }
        },
    )
    .await
}

async fn chatgpt_get_request_with_auth<T: DeserializeOwned>(
    config: &Config,
    path: String,
    request_auth: ChatGptRequestAuth,
) -> anyhow::Result<T> {
    let client = create_client();
    let url = format!("{}{}", config.chatgpt_base_url, path);
    let mut request = client
        .get(&url)
        .header("Authorization", request_auth.authorization())
        .header("chatgpt-account-id", request_auth.account_id())
        .header("Content-Type", "application/json")
        .timeout(DIRECTORY_CONNECTORS_TIMEOUT);
    if request_auth.is_fedramp_account() {
        request = request.header("X-OpenAI-Fedramp", "true");
    }
    let response = request.send().await.context("failed to send request")?;

    if response.status().is_success() {
        response
            .json()
            .await
            .context("failed to parse JSON response")
    } else {
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.unwrap_or_default();
        if let Some(summary) = summarize_connector_http_error_body(content_type.as_deref(), &body) {
            anyhow::bail!("request failed with status {status}: {summary}");
        }
        anyhow::bail!("request failed with status {status}");
    }
}

// Merge-safety anchor: connector-directory HTTP failures must stay status-first and concise so
// Cloudflare challenge pages or other large HTML bodies never dump into default codex-tui.log.
fn summarize_connector_http_error_body(content_type: Option<&str>, body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if connector_http_error_body_is_html(content_type, trimmed) {
        return Some(CONNECTOR_HTML_ERROR_BODY_OMITTED.to_string());
    }

    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(truncate_connector_http_error_preview(collapsed))
}

fn connector_http_error_body_is_html(content_type: Option<&str>, trimmed_body: &str) -> bool {
    let body_prefix = trimmed_body
        .chars()
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase();

    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/html"))
        || body_prefix.starts_with("<html")
        || body_prefix.starts_with("<!doctype html")
}

fn truncate_connector_http_error_preview(preview: String) -> String {
    if preview.chars().count() <= CONNECTOR_HTTP_ERROR_PREVIEW_LIMIT {
        preview
    } else {
        let truncated: String = preview
            .chars()
            .take(CONNECTOR_HTTP_ERROR_PREVIEW_LIMIT)
            .collect();
        format!("{truncated}...")
    }
}

pub(crate) fn accessible_connectors_from_mcp_tools(
    mcp_tools: &HashMap<String, ToolInfo>,
) -> Vec<AppInfo> {
    // ToolInfo already carries plugin provenance, so app-level plugin sources
    // can be derived here instead of requiring a separate enrichment pass.
    let tools = mcp_tools.values().filter_map(|tool| {
        if tool.server_name != CODEX_APPS_MCP_SERVER_NAME {
            return None;
        }
        let connector_id = tool.connector_id.as_deref()?;
        Some(codex_connectors::accessible::AccessibleConnectorTool {
            connector_id: connector_id.to_string(),
            connector_name: tool.connector_name.clone(),
            connector_description: tool.connector_description.clone(),
            plugin_display_names: tool.plugin_display_names.clone(),
        })
    });
    codex_connectors::accessible::collect_accessible_connectors(tools)
}

pub fn with_app_enabled_state(mut connectors: Vec<AppInfo>, config: &Config) -> Vec<AppInfo> {
    let user_apps_config = read_user_apps_config(config);
    let requirements_apps_config = config.config_layer_stack.requirements_toml().apps.as_ref();
    if user_apps_config.is_none() && requirements_apps_config.is_none() {
        return connectors;
    }

    for connector in &mut connectors {
        if let Some(apps_config) = user_apps_config.as_ref()
            && (apps_config.default.is_some()
                || apps_config.apps.contains_key(connector.id.as_str()))
        {
            connector.is_enabled = app_is_enabled(apps_config, Some(connector.id.as_str()));
        }

        if requirements_apps_config
            .and_then(|apps| apps.apps.get(connector.id.as_str()))
            .is_some_and(|app| app.enabled == Some(false))
        {
            connector.is_enabled = false;
        }
    }

    connectors
}

pub fn with_app_plugin_sources(
    mut connectors: Vec<AppInfo>,
    tool_plugin_provenance: &ToolPluginProvenance,
) -> Vec<AppInfo> {
    for connector in &mut connectors {
        connector.plugin_display_names = tool_plugin_provenance
            .plugin_display_names_for_connector_id(connector.id.as_str())
            .to_vec();
    }
    connectors
}

pub(crate) fn app_tool_policy(
    config: &Config,
    connector_id: Option<&str>,
    tool_name: &str,
    tool_title: Option<&str>,
    annotations: Option<&ToolAnnotations>,
) -> AppToolPolicy {
    let apps_config = read_apps_config(config);
    app_tool_policy_from_apps_config(
        apps_config.as_ref(),
        connector_id,
        tool_name,
        tool_title,
        annotations,
    )
}

pub(crate) fn codex_app_tool_is_enabled(config: &Config, tool_info: &ToolInfo) -> bool {
    if tool_info.server_name != CODEX_APPS_MCP_SERVER_NAME {
        return true;
    }

    app_tool_policy(
        config,
        tool_info.connector_id.as_deref(),
        &tool_info.tool.name,
        tool_info.tool.title.as_deref(),
        tool_info.tool.annotations.as_ref(),
    )
    .enabled
}

fn read_apps_config(config: &Config) -> Option<AppsConfigToml> {
    let apps_config = read_user_apps_config(config);
    let had_apps_config = apps_config.is_some();
    let mut apps_config = apps_config.unwrap_or_default();
    apply_requirements_apps_constraints(
        &mut apps_config,
        config.config_layer_stack.requirements_toml().apps.as_ref(),
    );
    if had_apps_config || apps_config.default.is_some() || !apps_config.apps.is_empty() {
        Some(apps_config)
    } else {
        None
    }
}

fn read_user_apps_config(config: &Config) -> Option<AppsConfigToml> {
    config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("apps"))
        .cloned()
        .and_then(|value| AppsConfigToml::deserialize(value).ok())
}

fn apply_requirements_apps_constraints(
    apps_config: &mut AppsConfigToml,
    requirements_apps_config: Option<&AppsRequirementsToml>,
) {
    let Some(requirements_apps_config) = requirements_apps_config else {
        return;
    };

    for (app_id, requirement) in &requirements_apps_config.apps {
        if requirement.enabled != Some(false) {
            continue;
        }
        let app = apps_config.apps.entry(app_id.clone()).or_default();
        app.enabled = false;
    }
}

fn app_is_enabled(apps_config: &AppsConfigToml, connector_id: Option<&str>) -> bool {
    let default_enabled = apps_config
        .default
        .as_ref()
        .map(|defaults| defaults.enabled)
        .unwrap_or(true);

    connector_id
        .and_then(|connector_id| apps_config.apps.get(connector_id))
        .map(|app| app.enabled)
        .unwrap_or(default_enabled)
}

fn app_tool_policy_from_apps_config(
    apps_config: Option<&AppsConfigToml>,
    connector_id: Option<&str>,
    tool_name: &str,
    tool_title: Option<&str>,
    annotations: Option<&ToolAnnotations>,
) -> AppToolPolicy {
    let Some(apps_config) = apps_config else {
        return AppToolPolicy::default();
    };

    let app = connector_id.and_then(|connector_id| apps_config.apps.get(connector_id));
    let tools = app.and_then(|app| app.tools.as_ref());
    let tool_config = tools.and_then(|tools| {
        tools
            .tools
            .get(tool_name)
            .or_else(|| tool_title.and_then(|title| tools.tools.get(title)))
    });
    let approval = tool_config
        .and_then(|tool| tool.approval_mode)
        .or_else(|| app.and_then(|app| app.default_tools_approval_mode))
        .unwrap_or(AppToolApproval::Auto);

    if !app_is_enabled(apps_config, connector_id) {
        return AppToolPolicy {
            enabled: false,
            approval,
        };
    }

    if let Some(enabled) = tool_config.and_then(|tool| tool.enabled) {
        return AppToolPolicy { enabled, approval };
    }

    if let Some(enabled) = app.and_then(|app| app.default_tools_enabled) {
        return AppToolPolicy { enabled, approval };
    }

    let app_defaults = apps_config.default.as_ref();
    let destructive_enabled = app
        .and_then(|app| app.destructive_enabled)
        .unwrap_or_else(|| {
            app_defaults
                .map(|defaults| defaults.destructive_enabled)
                .unwrap_or(true)
        });
    let open_world_enabled = app
        .and_then(|app| app.open_world_enabled)
        .unwrap_or_else(|| {
            app_defaults
                .map(|defaults| defaults.open_world_enabled)
                .unwrap_or(true)
        });
    let destructive_hint = annotations
        .and_then(|annotations| annotations.destructive_hint)
        .unwrap_or(true);
    let open_world_hint = annotations
        .and_then(|annotations| annotations.open_world_hint)
        .unwrap_or(true);
    let enabled =
        (destructive_enabled || !destructive_hint) && (open_world_enabled || !open_world_hint);

    AppToolPolicy { enabled, approval }
}

#[cfg(test)]
#[path = "connectors_tests.rs"]
mod tests;
