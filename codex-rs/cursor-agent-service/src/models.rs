use codex_models_manager::model_info::BASE_INSTRUCTIONS;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;

pub const COMPOSER_2_5_MODEL_ID: &str = "composer-2.5";
pub const GROK_4_5_HIGH_FAST_MODEL_ID: &str = "cursor-grok-4.5-high-fast";

pub fn static_model_catalog(
    context_window_tokens: i64,
    effective_context_window_percent: i64,
) -> ModelsResponse {
    ModelsResponse {
        models: vec![
            cursor_model(
                COMPOSER_2_5_MODEL_ID,
                "Composer 2.5",
                /*priority*/ 0,
                context_window_tokens,
                effective_context_window_percent,
            ),
            cursor_model(
                GROK_4_5_HIGH_FAST_MODEL_ID,
                "Grok 4.5 High Fast",
                /*priority*/ 1,
                context_window_tokens,
                effective_context_window_percent,
            ),
        ],
    }
}

fn cursor_model(
    slug: &str,
    display_name: &str,
    priority: i32,
    context_window_tokens: i64,
    effective_context_window_percent: i64,
) -> ModelInfo {
    ModelInfo {
        slug: slug.to_string(),
        display_name: display_name.to_string(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: Vec::new(),
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        availability_nux: None,
        upgrade: None,
        base_instructions: BASE_INSTRUCTIONS.to_string(),
        model_messages: None,
        include_skills_usage_instructions: true,
        supports_reasoning_summary_parameter: false,
        default_reasoning_summary: ReasoningSummary::None,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
        supports_parallel_tool_calls: true,
        supports_image_detail_original: false,
        context_window: Some(context_window_tokens),
        max_context_window: Some(context_window_tokens),
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent,
        experimental_supported_tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        used_fallback_model_metadata: false,
        supports_search_tool: false,
        use_responses_lite: false,
        auto_review_model_override: None,
        tool_mode: None,
        multi_agent_version: None,
    }
}
