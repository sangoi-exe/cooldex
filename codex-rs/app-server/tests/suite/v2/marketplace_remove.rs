use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::MarketplaceRemoveParams;
use codex_app_server_protocol::MarketplaceRemoveResponse;
use codex_app_server_protocol::PluginListParams;
use codex_app_server_protocol::PluginListResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_config::MarketplaceConfigUpdate;
use codex_config::record_user_marketplace;
use codex_core::plugins::marketplace_install_root;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn configured_marketplace_update() -> MarketplaceConfigUpdate<'static> {
    MarketplaceConfigUpdate {
        last_updated: "2026-04-13T00:00:00Z",
        last_revision: None,
        source_type: "git",
        source: "https://github.com/owner/repo.git",
        ref_name: Some("main"),
        sparse_paths: &[],
    }
}

fn write_plugins_enabled_config(codex_home: &std::path::Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"[features]
plugins = true

[plugins."sample@debug"]
enabled = true
"#,
    )?;
    Ok(())
}

fn write_installed_marketplace(codex_home: &std::path::Path, marketplace_name: &str) -> Result<()> {
    let root = marketplace_install_root(codex_home).join(marketplace_name);
    std::fs::create_dir_all(root.join(".agents/plugins"))?;
    std::fs::create_dir_all(root.join("plugins/sample/.codex-plugin"))?;
    std::fs::create_dir_all(root.join("plugins/sample/skills/sample-skill"))?;
    std::fs::write(
        root.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "{marketplace_name}",
  "plugins": [
    {{
      "name": "sample",
      "source": {{
        "source": "local",
        "path": "./plugins/sample"
      }}
    }}
  ]
}}"#
        ),
    )?;
    std::fs::write(
        root.join("plugins/sample/.codex-plugin/plugin.json"),
        r#"{"name":"sample"}"#,
    )?;
    std::fs::write(
        root.join("plugins/sample/skills/sample-skill/SKILL.md"),
        "---\nname: sample-skill\ndescription: sample marketplace skill\n---\n\n# Body\n",
    )?;
    Ok(())
}

fn canonicalize_path_with_existing_parent(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .with_context(|| format!("path {} should have a parent", path.display()))?;
    let file_name = path
        .file_name()
        .with_context(|| format!("path {} should have a file name", path.display()))?;

    Ok(parent.canonicalize()?.join(file_name))
}

#[tokio::test]
async fn marketplace_remove_deletes_config_and_installed_root() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_plugins_enabled_config(codex_home.path())?;
    record_user_marketplace(codex_home.path(), "debug", &configured_marketplace_update())?;
    write_installed_marketplace(codex_home.path(), "debug")?;
    let installed_root = marketplace_install_root(codex_home.path()).join("debug");

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_marketplace_remove_request(MarketplaceRemoveParams {
            marketplace_name: "debug".to_string(),
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: MarketplaceRemoveResponse = to_response(response)?;
    assert_eq!(response.marketplace_name, "debug");
    let removed_installed_root = response
        .installed_root
        .context("marketplace/remove should return removed installed root")?;
    assert_eq!(
        canonicalize_path_with_existing_parent(removed_installed_root.as_path())?,
        canonicalize_path_with_existing_parent(&installed_root)?,
    );

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config.contains("[marketplaces.debug]"));
    assert!(
        !marketplace_install_root(codex_home.path())
            .join("debug")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn marketplace_remove_updates_plugin_listing_but_not_skills_without_installed_plugin()
-> Result<()> {
    let codex_home = TempDir::new()?;
    write_plugins_enabled_config(codex_home.path())?;
    record_user_marketplace(codex_home.path(), "debug", &configured_marketplace_update())?;
    write_installed_marketplace(codex_home.path(), "debug")?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let plugin_list_request = mcp
        .send_plugin_list_request(PluginListParams { cwds: None })
        .await?;
    let plugin_list_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(plugin_list_request)),
    )
    .await??;
    let PluginListResponse { marketplaces, .. } = to_response(plugin_list_response)?;
    assert!(
        marketplaces.iter().any(|marketplace| {
            marketplace.name == "debug"
                && marketplace
                    .plugins
                    .iter()
                    .any(|plugin| plugin.id == "sample@debug")
        }),
        "plugin/list should be warm with the installed marketplace before removal"
    );

    let skills_list_request = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: false,
            per_cwd_extra_user_roots: None,
        })
        .await?;
    let skills_list_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(skills_list_request)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(skills_list_response)?;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "sample-skill"),
        "skills/list should stay empty without an installed plugin even when the marketplace exists"
    );

    let remove_request = mcp
        .send_marketplace_remove_request(MarketplaceRemoveParams {
            marketplace_name: "debug".to_string(),
        })
        .await?;
    let remove_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(remove_request)),
    )
    .await??;
    let MarketplaceRemoveResponse {
        marketplace_name, ..
    } = to_response(remove_response)?;
    assert_eq!(marketplace_name, "debug");

    let plugin_list_request = mcp
        .send_plugin_list_request(PluginListParams { cwds: None })
        .await?;
    let plugin_list_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(plugin_list_request)),
    )
    .await??;
    let PluginListResponse { marketplaces, .. } = to_response(plugin_list_response)?;
    assert!(
        marketplaces
            .iter()
            .all(|marketplace| marketplace.name != "debug"),
        "plugin/list should drop the removed marketplace without manual cache busting"
    );

    let skills_list_request = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: false,
            per_cwd_extra_user_roots: None,
        })
        .await?;
    let skills_list_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(skills_list_request)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(skills_list_response)?;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "sample-skill"),
        "skills/list should remain unchanged because marketplace removal does not uninstall plugins"
    );
    Ok(())
}

#[tokio::test]
async fn marketplace_remove_rejects_unknown_marketplace() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_marketplace_remove_request(MarketplaceRemoveParams {
            marketplace_name: "debug".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert_eq!(
        err.error.message,
        "marketplace `debug` is not configured or installed",
    );
    Ok(())
}
