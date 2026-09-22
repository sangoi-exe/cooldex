use super::*;
use crate::context::MultiAgentRoleInstructions;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::protocol::AgentUsageHintBinding;
use codex_protocol::protocol::AgentUsageHintInstructions;
use pretty_assertions::assert_eq;
use pretty_assertions::assert_ne;

const BASE_SECRET: &str = "base-instruction-secret";
const DEVELOPER_SECRET: &str = "developer-instruction-secret";
const HINT_SECRET: &str = "usage-hint-secret";

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
        BaseInstructions {
            text: BASE_SECRET.to_string(),
            provenance: Some(BaseInstructionsProvenance::Model {
                model: "gpt-5.4".to_string(),
            }),
        },
        Some(DEVELOPER_SECRET.to_string()),
        Some("priority".to_string()),
        Some(false),
        AgentUsageHintBinding::Inherited {
            instructions: Some(AgentUsageHintInstructions {
                text: HINT_SECRET.to_string(),
                marked: true,
            }),
        },
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
    different_base.base_instructions.text = "different base".to_string();
    let mut different_base_provenance = expected.clone();
    different_base_provenance.base_instructions.provenance =
        Some(BaseInstructionsProvenance::Custom);
    let mut different_developer = expected.clone();
    different_developer.developer_instructions = Some(Arc::from("different developer"));
    let mut different_tier = expected.clone();
    different_tier.service_tier = None;
    let mut different_shell_tool = expected.clone();
    different_shell_tool.shell_tool_enabled = Some(true);
    let mut different_usage_hint_binding = expected.clone();
    different_usage_hint_binding.agent_usage_hint_binding = AgentUsageHintBinding::Resolve;

    for actual in [
        different_role,
        different_provider_id,
        different_provider,
        different_model,
        different_effort,
        different_summary,
        different_base,
        different_base_provenance,
        different_developer,
        different_tier,
        different_shell_tool,
        different_usage_hint_binding,
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
    assert_eq!(config.base_instructions, Some(BASE_SECRET.to_string()));
    assert_eq!(
        config.base_instructions_provenance,
        Some(BaseInstructionsProvenance::Model {
            model: "gpt-5.4".to_string(),
        })
    );
    assert_eq!(
        config.agent_usage_hint_binding,
        AgentUsageHintBinding::Inherited {
            instructions: Some(AgentUsageHintInstructions {
                text: HINT_SECRET.to_string(),
                marked: true,
            }),
        }
    );
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
    assert!(!debug.contains(HINT_SECRET));
    assert!(!debug.contains("OpenAI"));
    assert!(debug.contains("<redacted>"));
}

// Merge-safety anchor: full-history identity tests retain the raw hint text/marker distinction
// and inherited absence without treating either state as ordinary mutable resolution.
#[test]
fn full_history_capture_preserves_typed_hint_or_inherited_absence() {
    let marked =
        snapshot().for_full_history(Some(MultiAgentRoleInstructions::catalog(HINT_SECRET)));
    assert_eq!(
        marked.agent_usage_hint_binding,
        AgentUsageHintBinding::Inherited {
            instructions: Some(AgentUsageHintInstructions {
                text: HINT_SECRET.to_string(),
                marked: true,
            }),
        }
    );

    let absent = snapshot().for_full_history(None);
    assert_eq!(
        absent.agent_usage_hint_binding,
        AgentUsageHintBinding::Inherited { instructions: None }
    );
}

#[test]
fn usage_hint_instruction_conversion_preserves_raw_text_and_markers() {
    for instructions in [
        MultiAgentRoleInstructions::unmarked("unmarked hint"),
        MultiAgentRoleInstructions::catalog("marked hint"),
    ] {
        let persisted = instructions.clone().into_agent_usage_hint_instructions();
        let restored =
            MultiAgentRoleInstructions::from_agent_usage_hint_instructions(persisted.clone());

        assert_eq!(restored.into_agent_usage_hint_instructions(), persisted);
    }
}
