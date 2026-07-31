use std::path::PathBuf;

use codex_cursor_agent_service::COMPOSER_2_5_MODEL_ID;
use codex_cursor_agent_service::CursorAgentServiceBackendConfig;
use codex_cursor_agent_service::GROK_4_5_HIGH_FAST_MODEL_ID;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::CursorAgentServiceProviderInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_models_manager::manager::RefreshStrategy;
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
use pretty_assertions::assert_eq;

use crate::ProviderAccountState;
use crate::ProviderCapabilities;
use crate::create_model_provider;

const EXPECTED_USER_ID: u64 = 390_777_501;
const EXPECTED_TEAM_ID: u64 = 12_565_657;
const EXPECTED_SERVICE_ORIGIN: &str = "https://agentn.global.api5.cursor.sh";
const CONTEXT_WINDOW_TOKENS: i64 = 65_536;
const EFFECTIVE_CONTEXT_WINDOW_PERCENT: i64 = 75;
const MAX_PENDING_TOOL_ACTIONS: usize = 8;

fn cursor_provider_info() -> ModelProviderInfo {
    ModelProviderInfo {
        name: "Cursor Corporate".to_string(),
        wire_api: WireApi::CursorAgentService,
        cursor_agent_service: Some(CursorAgentServiceProviderInfo {
            expected_user_id: EXPECTED_USER_ID,
            expected_team_id: EXPECTED_TEAM_ID,
            expected_service_origin: EXPECTED_SERVICE_ORIGIN.to_string(),
            context_window_tokens: CONTEXT_WINDOW_TOKENS,
            effective_context_window_percent: EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            max_pending_tool_actions: MAX_PENDING_TOOL_ACTIONS,
        }),
        ..ModelProviderInfo::default()
    }
}

fn expected_cursor_model(slug: &str, display_name: &str, priority: i32) -> ModelInfo {
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
        context_window: Some(CONTEXT_WINDOW_TOKENS),
        max_context_window: Some(CONTEXT_WINDOW_TOKENS),
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: EFFECTIVE_CONTEXT_WINDOW_PERCENT,
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

fn test_codex_home() -> PathBuf {
    std::env::temp_dir().join(format!(
        "codex-cursor-agent-service-provider-test-{}",
        std::process::id()
    ))
}

#[tokio::test]
async fn cursor_provider_discards_personal_chatgpt_auth() {
    let provider = create_model_provider(
        cursor_provider_info(),
        Some(AuthManager::from_auth_for_testing(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        )),
    );

    assert!(provider.auth_manager().is_none());
    assert_eq!(provider.auth().await, None);
    assert_eq!(provider.supports_attestation(), false);
    assert_eq!(
        provider.approval_review_preferred_model(),
        COMPOSER_2_5_MODEL_ID
    );
    assert_eq!(
        provider.memory_extraction_preferred_model(),
        COMPOSER_2_5_MODEL_ID
    );
    assert_eq!(
        provider.memory_consolidation_preferred_model(),
        COMPOSER_2_5_MODEL_ID
    );
    assert_eq!(
        provider.account_state(),
        Ok(ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
    );
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            namespace_tools: true,
            image_generation: false,
            web_search: false,
        }
    );

    let backend = provider
        .cursor_agent_service_backend()
        .expect("Cursor provider should expose its dedicated backend");
    assert_eq!(
        backend.config(),
        &CursorAgentServiceBackendConfig {
            expected_user_id: EXPECTED_USER_ID,
            expected_team_id: EXPECTED_TEAM_ID,
            expected_service_origin: EXPECTED_SERVICE_ORIGIN.to_string(),
            context_window_tokens: CONTEXT_WINDOW_TOKENS,
            effective_context_window_percent: EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            max_pending_tool_actions: MAX_PENDING_TOOL_ACTIONS,
        }
    );
}

#[tokio::test]
async fn cursor_provider_rejects_openai_api_and_auth_paths() {
    let provider = create_model_provider(
        cursor_provider_info(),
        Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
            "personal-openai-key",
        ))),
    );

    assert_eq!(
        provider.api_provider().await.unwrap_err().to_string(),
        "cursor_agent_service providers do not use the OpenAI-compatible API client"
    );
    let auth_error = match provider.api_auth().await {
        Ok(_) => panic!("Cursor provider must reject the generic auth path"),
        Err(error) => error,
    };
    assert_eq!(
        auth_error.to_string(),
        "cursor_agent_service providers do not use OpenAI or ChatGPT authentication"
    );
}

#[tokio::test]
async fn cursor_provider_uses_only_the_authoritative_static_catalog() {
    let provider = create_model_provider(cursor_provider_info(), /*auth_manager*/ None);
    let configured_override = ModelsResponse {
        models: vec![codex_models_manager::model_info::model_info_from_slug(
            "unexpected-configured-model",
        )],
    };

    let manager = provider.models_manager(test_codex_home(), Some(configured_override.clone()));
    let uncached_manager = provider.models_manager_without_cache(Some(configured_override));
    let catalog = manager
        .raw_model_catalog(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await;
    let uncached_catalog = uncached_manager
        .raw_model_catalog(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await;
    let expected = ModelsResponse {
        models: vec![
            expected_cursor_model(COMPOSER_2_5_MODEL_ID, "Composer 2.5", /*priority*/ 0),
            expected_cursor_model(
                GROK_4_5_HIGH_FAST_MODEL_ID,
                "Grok 4.5 High Fast",
                /*priority*/ 1,
            ),
        ],
    };

    assert_eq!(catalog, expected);
    assert_eq!(uncached_catalog, expected);

    let available_models = manager
        .list_models(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await;
    assert_eq!(
        available_models
            .iter()
            .map(|model| (model.model.as_str(), model.is_default))
            .collect::<Vec<_>>(),
        vec![
            (COMPOSER_2_5_MODEL_ID, true),
            (GROK_4_5_HIGH_FAST_MODEL_ID, false)
        ]
    );
}

#[test]
fn responses_provider_exposes_no_cursor_backend() {
    let provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        /*auth_manager*/ None,
    );

    assert!(provider.cursor_agent_service_backend().is_none());
}
