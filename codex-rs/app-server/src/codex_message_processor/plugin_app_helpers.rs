use std::collections::HashSet;

use anyhow::Result;
use codex_app_server_protocol::AppInfo;
use codex_app_server_protocol::AppSummary;
use codex_chatgpt::connectors;
use codex_core::config::Config;
use codex_core::plugins::AppConnectorId;
use codex_login::AuthManager;

pub(super) async fn load_plugin_app_summaries(
    config: &Config,
    auth_manager: &AuthManager,
    plugin_apps: &[AppConnectorId],
) -> Result<Vec<AppSummary>> {
    if plugin_apps.is_empty() {
        return Ok(Vec::new());
    }

    let auth_snapshot = connectors::load_connector_auth_snapshot(auth_manager).await?;
    let connectors = match connectors::list_all_connectors_with_options(
        config,
        auth_snapshot.as_ref(),
        /*force_refetch*/ false,
    )
    .await
    {
        Ok(connectors) => connectors,
        Err(err) => {
            tracing::warn!("failed to load app metadata for plugin/read: {err:#}");
            connectors::list_cached_all_connectors(config, auth_snapshot.as_ref())
                .await
                .unwrap_or_default()
        }
    };

    let plugin_connectors = connectors::connectors_for_plugin_apps(connectors, plugin_apps);

    // Merge-safety anchor: app metadata access checks must use the app-server
    // AccountManager owner passed into this request, not a hidden AuthManager.
    let auth = auth_snapshot
        .as_ref()
        .map(codex_login::ChatGptAuthContext::request_auth);
    let accessible_connectors =
        match connectors::list_accessible_connectors_from_mcp_tools_with_options_and_status(
            config, auth, /*force_refetch*/ false,
        )
        .await
        {
            Ok(status) if status.codex_apps_ready => status.connectors,
            Ok(_) => {
                anyhow::bail!("codex_apps MCP is not ready for plugin/read app auth state");
            }
            Err(err) => {
                return Err(err.context("failed to load app auth state for plugin/read"));
            }
        };

    let accessible_ids = accessible_connectors
        .iter()
        .map(|connector| connector.id.as_str())
        .collect::<HashSet<_>>();

    Ok(plugin_connectors
        .into_iter()
        .map(|connector| {
            let needs_auth = !accessible_ids.contains(connector.id.as_str());
            AppSummary {
                id: connector.id,
                name: connector.name,
                description: connector.description,
                install_url: connector.install_url,
                needs_auth,
            }
        })
        .collect())
}

pub(super) fn plugin_apps_needing_auth(
    all_connectors: &[AppInfo],
    accessible_connectors: &[AppInfo],
    plugin_apps: &[AppConnectorId],
    codex_apps_ready: bool,
) -> Vec<AppSummary> {
    if !codex_apps_ready {
        return Vec::new();
    }

    let accessible_ids = accessible_connectors
        .iter()
        .map(|connector| connector.id.as_str())
        .collect::<HashSet<_>>();
    let plugin_app_ids = plugin_apps
        .iter()
        .map(|connector_id| connector_id.0.as_str())
        .collect::<HashSet<_>>();

    all_connectors
        .iter()
        .filter(|connector| {
            plugin_app_ids.contains(connector.id.as_str())
                && !accessible_ids.contains(connector.id.as_str())
        })
        .cloned()
        .map(|connector| AppSummary {
            id: connector.id,
            name: connector.name,
            description: connector.description,
            install_url: connector.install_url,
            needs_auth: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::AppInfo;
    use codex_core::plugins::AppConnectorId;
    use pretty_assertions::assert_eq;

    use super::plugin_apps_needing_auth;

    #[test]
    fn plugin_apps_needing_auth_returns_empty_when_codex_apps_is_not_ready() {
        let all_connectors = vec![AppInfo {
            id: "alpha".to_string(),
            name: "Alpha".to_string(),
            description: Some("Alpha connector".to_string()),
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://chatgpt.com/apps/alpha/alpha".to_string()),
            is_accessible: false,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        }];

        assert_eq!(
            plugin_apps_needing_auth(
                &all_connectors,
                &[],
                &[AppConnectorId("alpha".to_string())],
                /*codex_apps_ready*/ false,
            ),
            Vec::new()
        );
    }
}
