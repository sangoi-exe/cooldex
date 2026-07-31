//! Fork-owned transport for the pinned Cursor AgentService sampling protocol.

mod auth;
mod client;
mod models;

/// Immutable provider configuration owned by the Cursor AgentService backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorAgentServiceBackendConfig {
    pub expected_user_id: u64,
    pub expected_team_id: u64,
    pub expected_service_origin: String,
    pub context_window_tokens: i64,
    pub effective_context_window_percent: i64,
    pub max_pending_tool_actions: usize,
}

/// Fork-owned runtime entry point for Cursor AgentService sampling.
#[derive(Debug)]
pub struct CursorAgentServiceBackend {
    config: CursorAgentServiceBackendConfig,
}

impl CursorAgentServiceBackend {
    pub fn new(config: CursorAgentServiceBackendConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &CursorAgentServiceBackendConfig {
        &self.config
    }
}

pub use auth::CursorCredentialStore;
pub use auth::CursorCredentialStoreError;
pub use auth::CursorCredentials;
pub use client::AgentServiceRun;
pub use client::AgentServiceTransport;
pub use client::AgentServiceTransportError;
pub use models::COMPOSER_2_5_MODEL_ID;
pub use models::GROK_4_5_HIGH_FAST_MODEL_ID;
pub use models::static_model_catalog;
pub mod proto;
