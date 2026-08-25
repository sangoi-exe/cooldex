use super::*;
use codex_features::Feature;
use codex_protocol::ThreadId;
use pretty_assertions::assert_ne;

const BASE_SECRET: &str = "base-instruction-secret";
const DEVELOPER_SECRET: &str = "developer-instruction-secret";

fn snapshot() -> AgentIdentitySnapshot {
    AgentIdentitySnapshot::capture(
        Some("worker".to_string()),
        "openai".to_string(),
        ModelProviderInfo {
            name: "OpenAI".to_string(),
            ..Default::default()
        },
        "gpt-5.4".to_string(),
        Some(ReasoningEffort::High),
        Some(ReasoningSummary::Detailed),
        BASE_SECRET.to_string(),
        Some(DEVELOPER_SECRET.to_string()),
        Some("priority".to_string()),
        Some(false),
    )
}

fn thread_spawn_source() -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth: 0,
        agent_path: None,
        agent_nickname: None,
        agent_role: Some("worker".to_string()),
    })
}

#[test]
fn identity_equality_covers_every_field() {
    let expected = snapshot();

    let mut different_role = expected.clone();
    different_role.agent_role = Some("reviewer".to_string());
    let mut different_provider_id = expected.clone();
    different_provider_id.model_provider.id = "azure".to_string();
    let mut different_provider = expected.clone();
    different_provider.model_provider.info.name = "Different provider".to_string();
    let mut different_model = expected.clone();
    different_model.model = "gpt-5.5".to_string();
    let mut different_effort = expected.clone();
    different_effort.model_reasoning_effort = Some(ReasoningEffort::Low);
    let mut different_summary = expected.clone();
    different_summary.model_reasoning_summary = Some(ReasoningSummary::Concise);
    let mut different_base = expected.clone();
    different_base.base_instructions = Arc::from("different base");
    let mut different_developer = expected.clone();
    different_developer.developer_instructions = Some(Arc::from("different developer"));
    let mut different_tier = expected.clone();
    different_tier.service_tier = None;
    let mut different_shell_tool = expected.clone();
    different_shell_tool.shell_tool_enabled = Some(true);

    for actual in [
        different_role,
        different_provider_id,
        different_provider,
        different_model,
        different_effort,
        different_summary,
        different_base,
        different_developer,
        different_tier,
        different_shell_tool,
    ] {
        assert_ne!(actual, expected);
    }
}

#[tokio::test]
async fn identity_apply_restores_persisted_shell_tool_state() {
    let mut config = crate::config::test_config().await;
    config
        .features
        .enable(Feature::ShellTool)
        .expect("test config should enable shell tool");
    let mut session_source = thread_spawn_source();

    snapshot()
        .apply(&mut config, &mut session_source)
        .expect("identity should apply");

    assert!(!config.features.enabled(Feature::ShellTool));
}

#[tokio::test]
async fn identity_apply_preserves_reload_shell_tool_state_when_missing() {
    let mut config = crate::config::test_config().await;
    config
        .features
        .enable(Feature::ShellTool)
        .expect("test config should enable shell tool");
    let mut identity = snapshot();
    identity.shell_tool_enabled = None;
    let mut session_source = thread_spawn_source();

    identity
        .apply(&mut config, &mut session_source)
        .expect("identity should apply");

    assert!(config.features.enabled(Feature::ShellTool));
}

#[test]
fn identity_debug_redacts_instruction_and_provider_details() {
    let debug = format!("{:?}", snapshot());

    assert!(!debug.contains(BASE_SECRET));
    assert!(!debug.contains(DEVELOPER_SECRET));
    assert!(!debug.contains("OpenAI"));
    assert!(debug.contains("<redacted>"));
}
