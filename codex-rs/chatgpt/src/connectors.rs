use std::collections::HashSet;
use std::time::Duration;

use crate::chatgpt_client::chatgpt_get_request_with_auth;

use codex_app_server_protocol::AppInfo;
use codex_connectors::AllConnectorsCacheKey;
use codex_connectors::DirectoryListResponse;
use codex_connectors::filter::filter_disallowed_connectors;
use codex_connectors::merge::merge_connectors;
use codex_connectors::merge::merge_plugin_connectors;
use codex_core::config::Config;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools_with_environment_manager;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools_with_options;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools_with_options_and_status;
pub use codex_core::connectors::list_cached_accessible_connectors_from_mcp_tools;
pub use codex_core::connectors::with_app_enabled_state;
use codex_core::plugins::AppConnectorId;
use codex_core::plugins::PluginsManager;
use codex_login::AccountRuntimeLoadError;
use codex_login::AuthManager;
use codex_login::ChatGptAuthContext;
use codex_login::ChatGptRequestAuth;
use codex_login::default_client::originator;

const DIRECTORY_CONNECTORS_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn load_connector_auth_snapshot(
    auth_manager: &AuthManager,
) -> Result<Option<ChatGptAuthContext>, AccountRuntimeLoadError> {
    auth_manager.chatgpt_auth().await
}

fn apps_enabled(config: &Config, auth_snapshot: Option<&ChatGptAuthContext>) -> bool {
    config
        .features
        .apps_enabled_for_auth(auth_snapshot.is_some())
}

pub async fn list_connectors(
    config: &Config,
    auth_manager: &AuthManager,
) -> anyhow::Result<Vec<AppInfo>> {
    let auth_snapshot = load_connector_auth_snapshot(auth_manager).await?;
    let request_auth = auth_snapshot.as_ref().map(ChatGptAuthContext::request_auth);
    if !apps_enabled(config, auth_snapshot.as_ref()) {
        return Ok(Vec::new());
    }
    let (connectors_result, accessible_result) = tokio::join!(
        list_all_connectors(config, auth_snapshot.as_ref()),
        list_accessible_connectors_from_mcp_tools(config, request_auth),
    );
    let connectors = connectors_result?;
    let accessible = accessible_result?;
    Ok(with_app_enabled_state(
        merge_connectors_with_accessible(
            connectors, accessible, /*all_connectors_loaded*/ true,
        ),
        config,
    ))
}

pub async fn list_all_connectors(
    config: &Config,
    auth_snapshot: Option<&ChatGptAuthContext>,
) -> anyhow::Result<Vec<AppInfo>> {
    list_all_connectors_with_options(config, auth_snapshot, /*force_refetch*/ false).await
}

pub async fn list_cached_all_connectors(
    config: &Config,
    auth_snapshot: Option<&ChatGptAuthContext>,
) -> Option<Vec<AppInfo>> {
    if !apps_enabled(config, auth_snapshot) {
        return Some(Vec::new());
    }

    // Merge-safety anchor: cached connector reads use the same config-aware
    // request-auth snapshot as network connector fetches; do not reintroduce a
    // hidden AccountManager or process-global token/bootstrap path.
    let request_auth = auth_snapshot?.request_auth();
    let cache_key = all_connectors_cache_key(config, request_auth);
    let connectors = codex_connectors::cached_all_connectors(&cache_key)?;
    let connectors = merge_plugin_connectors(
        connectors,
        plugin_apps_for_config(config)
            .await
            .into_iter()
            .map(|connector_id| connector_id.0),
    );
    Some(filter_disallowed_connectors(
        connectors,
        originator().value.as_str(),
    ))
}

pub async fn list_all_connectors_with_options(
    config: &Config,
    auth_snapshot: Option<&ChatGptAuthContext>,
    force_refetch: bool,
) -> anyhow::Result<Vec<AppInfo>> {
    if !apps_enabled(config, auth_snapshot) {
        return Ok(Vec::new());
    }
    // Merge-safety anchor: connector network fetches must keep ChatGPT token
    // snapshots aligned with the caller's AuthManager so account-state leases,
    // cache keys, and request headers do not split across connector surfaces.
    let request_auth = auth_snapshot
        .ok_or_else(|| anyhow::anyhow!("ChatGPT connector auth snapshot not available"))?
        .request_auth();
    let cache_key = all_connectors_cache_key(config, request_auth);
    let request_auth = request_auth.clone();
    let connectors = codex_connectors::list_all_connectors_with_options(
        cache_key,
        request_auth.is_workspace_account(),
        force_refetch,
        |path| {
            let request_auth = request_auth.clone();
            async move {
                chatgpt_get_request_with_auth::<DirectoryListResponse>(
                    config,
                    path,
                    Some(DIRECTORY_CONNECTORS_TIMEOUT),
                    request_auth,
                )
                .await
            }
        },
    )
    .await?;
    let connectors = merge_plugin_connectors(
        connectors,
        plugin_apps_for_config(config)
            .await
            .into_iter()
            .map(|connector_id| connector_id.0),
    );
    Ok(filter_disallowed_connectors(
        connectors,
        originator().value.as_str(),
    ))
}

fn all_connectors_cache_key(
    config: &Config,
    request_auth: &ChatGptRequestAuth,
) -> AllConnectorsCacheKey {
    AllConnectorsCacheKey::new(
        config.chatgpt_base_url.clone(),
        Some(request_auth.account_id().to_string()),
        request_auth.chatgpt_user_id().map(str::to_string),
        request_auth.is_workspace_account(),
    )
}

async fn plugin_apps_for_config(config: &Config) -> Vec<codex_core::plugins::AppConnectorId> {
    PluginsManager::new(config.codex_home.to_path_buf())
        .plugins_for_config(config)
        .await
        .effective_apps()
}

pub fn connectors_for_plugin_apps(
    connectors: Vec<AppInfo>,
    plugin_apps: &[AppConnectorId],
) -> Vec<AppInfo> {
    let plugin_app_ids = plugin_apps
        .iter()
        .map(|connector_id| connector_id.0.as_str())
        .collect::<HashSet<_>>();

    let connectors = merge_plugin_connectors(
        connectors,
        plugin_apps
            .iter()
            .map(|connector_id| connector_id.0.clone()),
    );
    filter_disallowed_connectors(connectors, originator().value.as_str())
        .into_iter()
        .filter(|connector| plugin_app_ids.contains(connector.id.as_str()))
        .collect()
}

pub fn merge_connectors_with_accessible(
    connectors: Vec<AppInfo>,
    accessible_connectors: Vec<AppInfo>,
    all_connectors_loaded: bool,
) -> Vec<AppInfo> {
    let accessible_connectors = if all_connectors_loaded {
        let connector_ids: HashSet<&str> = connectors
            .iter()
            .map(|connector| connector.id.as_str())
            .collect();
        accessible_connectors
            .into_iter()
            .filter(|connector| connector_ids.contains(connector.id.as_str()))
            .collect()
    } else {
        accessible_connectors
    };
    let merged = merge_connectors(connectors, accessible_connectors);
    filter_disallowed_connectors(merged, originator().value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_connectors::metadata::connector_install_url;
    use codex_core::plugins::AppConnectorId;
    use pretty_assertions::assert_eq;

    fn app(id: &str) -> AppInfo {
        AppInfo {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: None,
            is_accessible: false,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        }
    }

    fn merged_app(id: &str, is_accessible: bool) -> AppInfo {
        AppInfo {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some(connector_install_url(id, id)),
            is_accessible,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        }
    }

    #[test]
    fn excludes_accessible_connectors_not_in_all_when_all_loaded() {
        let merged = merge_connectors_with_accessible(
            vec![app("alpha")],
            vec![app("alpha"), app("beta")],
            /*all_connectors_loaded*/ true,
        );
        assert_eq!(merged, vec![merged_app("alpha", /*is_accessible*/ true)]);
    }

    #[test]
    fn keeps_accessible_connectors_not_in_all_while_all_loading() {
        let merged = merge_connectors_with_accessible(
            vec![app("alpha")],
            vec![app("alpha"), app("beta")],
            /*all_connectors_loaded*/ false,
        );
        assert_eq!(
            merged,
            vec![
                merged_app("alpha", /*is_accessible*/ true),
                merged_app("beta", /*is_accessible*/ true)
            ]
        );
    }

    #[test]
    fn connectors_for_plugin_apps_returns_only_requested_plugin_apps() {
        let connectors = connectors_for_plugin_apps(
            vec![app("alpha"), app("beta")],
            &[
                AppConnectorId("alpha".to_string()),
                AppConnectorId("gmail".to_string()),
            ],
        );
        assert_eq!(
            connectors,
            vec![app("alpha"), merged_app("gmail", /*is_accessible*/ false)]
        );
    }

    #[test]
    fn connectors_for_plugin_apps_filters_disallowed_plugin_apps() {
        let connectors = connectors_for_plugin_apps(
            Vec::new(),
            &[AppConnectorId(
                "asdk_app_6938a94a61d881918ef32cb999ff937c".to_string(),
            )],
        );
        assert_eq!(connectors, Vec::<AppInfo>::new());
    }
}
