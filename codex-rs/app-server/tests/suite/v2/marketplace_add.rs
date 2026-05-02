use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::MarketplaceAddParams;
use codex_app_server_protocol::MarketplaceAddResponse;
use codex_app_server_protocol::PluginListParams;
use codex_app_server_protocol::PluginListResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn write_plugins_enabled_config(codex_home: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"[features]
plugins = true

[plugins."sample@debug"]
enabled = true
"#,
    )
}

fn write_marketplace_source_with_plugin_and_skill(source: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(source.join(".agents/plugins"))?;
    std::fs::create_dir_all(source.join("plugins/sample/.codex-plugin"))?;
    std::fs::create_dir_all(source.join("plugins/sample/skills/sample-skill"))?;
    std::fs::write(
        source.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "sample",
      "source": {
        "source": "local",
        "path": "./plugins/sample"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        source.join("plugins/sample/.codex-plugin/plugin.json"),
        r#"{"name":"sample"}"#,
    )?;
    std::fs::write(
        source.join("plugins/sample/skills/sample-skill/SKILL.md"),
        "---\nname: sample-skill\ndescription: sample marketplace skill\n---\n\n# Body\n",
    )?;
    std::fs::write(source.join("plugins/sample/marker.txt"), "local ref")?;
    Ok(())
}

#[tokio::test]
async fn marketplace_add_local_directory_source() -> Result<()> {
    let codex_home = TempDir::new()?;
    let source = codex_home.path().join("marketplace");
    write_marketplace_source_with_plugin_and_skill(&source)?;
    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_marketplace_add_request(MarketplaceAddParams {
            source: "./marketplace".to_string(),
            ref_name: None,
            sparse_paths: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let MarketplaceAddResponse {
        marketplace_name,
        installed_root,
        already_added,
    } = to_response(response)?;
    let expected_root = AbsolutePathBuf::from_absolute_path(source.canonicalize()?)?;

    assert_eq!(marketplace_name, "debug");
    assert_eq!(installed_root, expected_root);
    assert!(!already_added);
    assert_eq!(
        std::fs::read_to_string(installed_root.as_path().join("plugins/sample/marker.txt"))?,
        "local ref"
    );
    Ok(())
}

#[tokio::test]
async fn marketplace_add_updates_plugin_listing_but_not_skills_without_installed_plugin()
-> Result<()> {
    let codex_home = TempDir::new()?;
    write_plugins_enabled_config(codex_home.path())?;
    let source = codex_home.path().join("marketplace");
    write_marketplace_source_with_plugin_and_skill(&source)?;

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
    let PluginListResponse {
        marketplaces,
        marketplace_load_errors,
        ..
    } = to_response(plugin_list_response)?;
    assert!(
        marketplaces
            .iter()
            .all(|marketplace| marketplace.name != "debug"),
        "debug marketplace should not exist before add"
    );
    assert!(
        marketplace_load_errors.is_empty(),
        "unexpected marketplace load errors before add: {marketplace_load_errors:?}"
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
        "sample-skill should not exist before add"
    );

    let add_request = mcp
        .send_marketplace_add_request(MarketplaceAddParams {
            source: "./marketplace".to_string(),
            ref_name: None,
            sparse_paths: None,
        })
        .await?;
    let add_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(add_request)),
    )
    .await??;
    let MarketplaceAddResponse {
        marketplace_name, ..
    } = to_response(add_response)?;
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
        marketplaces.iter().any(|marketplace| {
            marketplace.name == "debug"
                && marketplace
                    .plugins
                    .iter()
                    .any(|plugin| plugin.id == "sample@debug")
        }),
        "plugin/list should reflect the newly added marketplace without manual cache busting"
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
        "skills/list should stay empty until the newly added plugin is actually installed"
    );
    Ok(())
}
