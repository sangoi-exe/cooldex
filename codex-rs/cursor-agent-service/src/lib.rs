//! Fork-owned transport for the pinned Cursor AgentService sampling protocol.

mod auth;
mod backend;
mod client;
mod mapping;
mod models;
mod session;
#[cfg(test)]
mod test_support;

pub use auth::CURSOR_AGENT_SERVICE_ORIGIN;
pub use auth::CURSOR_DASHBOARD_ORIGIN;
pub use auth::CursorCredentialStore;
pub use auth::CursorCredentialStoreError;
pub use auth::CursorCredentials;
pub use auth::CursorIdentityError;
pub use auth::CursorRequestAuthError;
use auth::verify_cursor_identity_at;
pub use backend::CursorAgentServiceBackend;
pub use backend::CursorAgentServiceBackendConfig;
pub use backend::CursorAgentServiceBackendError;
pub use client::AgentServiceRun;
pub use client::AgentServiceTransport;
pub use client::AgentServiceTransportError;
pub use mapping::AcceptedLiveToolCall;
pub use mapping::COOLDEX_BASE_INSTRUCTIONS_RULE_PATH;
pub use mapping::COOLDEX_MCP_SERVER_IDENTIFIER;
pub use mapping::CompletedLiveToolCall;
pub use mapping::CursorMappingError;
pub use mapping::CursorSamplingRequest;
pub use mapping::CursorToolCallTracker;
pub use mapping::CursorToolSnapshot;
pub use mapping::MappedCursorRunRequest;
pub use mapping::build_request_context;
pub use mapping::map_interaction_update;
pub use mapping::map_request_context_result;
pub use mapping::map_sampling_request;
pub use models::COMPOSER_2_5_MODEL_ID;
pub use models::GROK_4_5_HIGH_FAST_MODEL_ID;
pub use models::static_model_catalog;
pub use session::CursorAgentServiceSessionError;
pub use session::CursorSamplingSession;
pub mod proto;
