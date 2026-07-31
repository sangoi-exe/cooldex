use std::path::PathBuf;
use std::sync::Arc;

use codex_api::Provider;
use codex_api::SharedAuthProvider;
use codex_cursor_agent_service::COMPOSER_2_5_MODEL_ID;
use codex_cursor_agent_service::CursorAgentServiceBackend;
use codex_cursor_agent_service::CursorAgentServiceBackendConfig;
use codex_cursor_agent_service::static_model_catalog;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelsResponse;

use crate::provider::ModelProvider;
use crate::provider::ModelProviderFuture;
use crate::provider::ProviderAccountResult;
use crate::provider::ProviderAccountState;
use crate::provider::ProviderCapabilities;

const CURSOR_API_CLIENT_ERROR: &str =
    "cursor_agent_service providers do not use the OpenAI-compatible API client";
const CURSOR_AUTH_ERROR: &str =
    "cursor_agent_service providers do not use OpenAI or ChatGPT authentication";

#[derive(Debug)]
pub(crate) struct CursorAgentServiceModelProvider {
    info: ModelProviderInfo,
    backend: Arc<CursorAgentServiceBackend>,
}

impl CursorAgentServiceModelProvider {
    pub(crate) fn new(info: ModelProviderInfo) -> Self {
        let config = info
            .cursor_agent_service
            .as_ref()
            .unwrap_or_else(|| panic!("Cursor AgentService provider requires validated config"));
        let backend = Arc::new(CursorAgentServiceBackend::new(
            CursorAgentServiceBackendConfig {
                expected_user_id: config.expected_user_id,
                expected_team_id: config.expected_team_id,
                expected_service_origin: config.expected_service_origin.clone(),
                context_window_tokens: config.context_window_tokens,
                effective_context_window_percent: config.effective_context_window_percent,
                max_pending_tool_actions: config.max_pending_tool_actions,
            },
        ));
        Self { info, backend }
    }

    fn model_catalog(&self) -> ModelsResponse {
        let config = self.backend.config();
        static_model_catalog(
            config.context_window_tokens,
            config.effective_context_window_percent,
        )
    }
}

impl ModelProvider for CursorAgentServiceModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            namespace_tools: true,
            image_generation: false,
            web_search: false,
        }
    }

    fn approval_review_preferred_model(&self) -> &'static str {
        COMPOSER_2_5_MODEL_ID
    }

    fn memory_extraction_preferred_model(&self) -> &'static str {
        COMPOSER_2_5_MODEL_ID
    }

    fn memory_consolidation_preferred_model(&self) -> &'static str {
        COMPOSER_2_5_MODEL_ID
    }

    fn cursor_agent_service_backend(&self) -> Option<Arc<CursorAgentServiceBackend>> {
        Some(self.backend.clone())
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        None
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        Box::pin(async { None })
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, Result<Provider>> {
        Box::pin(async {
            Err(CodexErr::InvalidRequest(
                CURSOR_API_CLIENT_ERROR.to_string(),
            ))
        })
    }

    fn api_auth(&self) -> ModelProviderFuture<'_, Result<SharedAuthProvider>> {
        Box::pin(async { Err(CodexErr::InvalidRequest(CURSOR_AUTH_ERROR.to_string())) })
    }

    fn models_manager(
        &self,
        _codex_home: PathBuf,
        _config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            /*auth_manager*/ None,
            self.model_catalog(),
        ))
    }

    fn models_manager_without_cache(
        &self,
        _config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            /*auth_manager*/ None,
            self.model_catalog(),
        ))
    }
}
