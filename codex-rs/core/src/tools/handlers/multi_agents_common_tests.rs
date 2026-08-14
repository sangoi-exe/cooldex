use super::*;
use codex_protocol::openai_models::ModelServiceTier;
use pretty_assertions::assert_eq;

fn model_preset(model: &str, multi_agent_version: MultiAgentVersion) -> ModelPreset {
    ModelPreset {
        id: model.to_string(),
        model: model.to_string(),
        display_name: model.to_string(),
        description: format!("{model} description"),
        model_specialty: None,
        default_reasoning_effort: ReasoningEffort::Medium,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: "Balanced".to_string(),
        }],
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: vec![ModelServiceTier {
            id: "priority".to_string(),
            name: "Fast".to_string(),
            description: "1.5x speed, increased usage".to_string(),
        }],
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        multi_agent_version: Some(multi_agent_version),
        availability_nux: None,
        supported_in_api: true,
        input_modalities: Vec::new(),
    }
}

fn available_models() -> Vec<ModelPreset> {
    vec![
        model_preset("visible-model", MultiAgentVersion::V2),
        model_preset("legacy-model", MultiAgentVersion::V1),
        model_preset("disabled-model", MultiAgentVersion::Disabled),
    ]
}

fn unavailable_model_error(requested_model: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!(
        "Unknown model `{requested_model}` for spawn_agent. Available models: visible-model, legacy-model"
    ))
}

#[test]
fn spawn_agent_model_selection_rejects_unknown_model() {
    assert_eq!(
        find_spawn_agent_model_name(&available_models(), "unknown-model", MultiAgentVersion::V2,),
        Err(unavailable_model_error("unknown-model"))
    );
}

#[test]
fn spawn_agent_model_selection_rejects_disabled_model() {
    assert_eq!(
        find_spawn_agent_model_name(&available_models(), "disabled-model", MultiAgentVersion::V2,),
        Err(unavailable_model_error("disabled-model"))
    );
}
