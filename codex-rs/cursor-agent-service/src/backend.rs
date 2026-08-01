use crate::AgentServiceTransport;
use crate::AgentServiceTransportError;
use crate::CURSOR_AGENT_SERVICE_ORIGIN;
use crate::CURSOR_DASHBOARD_ORIGIN;
use crate::CursorAgentServiceSessionError;
use crate::CursorCredentialStore;
use crate::CursorCredentialStoreError;
use crate::CursorCredentials;
use crate::CursorIdentityError;
use crate::CursorMappingError;
use crate::CursorSamplingRequest;
use crate::CursorSamplingSession;
use crate::map_sampling_request;
use crate::verify_cursor_identity_at;
use codex_protocol::ResponseItemId;
#[cfg(test)]
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

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
    runtime: BackendRuntime,
    auth_retry_lock: Mutex<()>,
}

impl CursorAgentServiceBackend {
    pub fn new(config: CursorAgentServiceBackendConfig) -> Self {
        let service_origin = config.expected_service_origin.clone();
        Self {
            config,
            runtime: BackendRuntime {
                credential_source: CredentialSource::Environment,
                dashboard_origin: CURSOR_DASHBOARD_ORIGIN.to_string(),
                service_origin,
            },
            auth_retry_lock: Mutex::new(()),
        }
    }

    pub fn config(&self) -> &CursorAgentServiceBackendConfig {
        &self.config
    }

    pub async fn start_sampling(
        &self,
        request: CursorSamplingRequest<'_>,
        consumer_dropped: CancellationToken,
    ) -> Result<CursorSamplingSession, CursorAgentServiceBackendError> {
        if self.config.expected_service_origin != CURSOR_AGENT_SERVICE_ORIGIN {
            return Err(CursorAgentServiceBackendError::UnexpectedServiceOrigin {
                expected: CURSOR_AGENT_SERVICE_ORIGIN,
                actual: self.config.expected_service_origin.clone(),
            });
        }

        let base_instructions = request.base_instructions.to_string();
        let mapped = map_sampling_request(request)?;
        let response_id = ResponseItemId::new("resp").to_string();
        let store = self.runtime.credential_store()?;
        let credentials = store.load()?;
        let first_attempt = self
            .open_authenticated_run(&mapped.request, &credentials)
            .await;
        drop(credentials);
        let run = match first_attempt {
            Ok(run) => run,
            Err(error) if error.is_unauthenticated() => {
                let _retry_guard = self.auth_retry_lock.lock().await;
                let refreshed_credentials = store.load()?;
                self.open_authenticated_run(&mapped.request, &refreshed_credentials)
                    .await?
            }
            Err(error) => return Err(error),
        };

        Ok(CursorSamplingSession::start(
            run,
            mapped.tool_snapshot,
            base_instructions,
            response_id,
            self.config.max_pending_tool_actions,
            consumer_dropped,
        ))
    }

    async fn open_authenticated_run(
        &self,
        request: &crate::proto::AgentRunRequest,
        credentials: &CursorCredentials,
    ) -> Result<crate::AgentServiceRun, CursorAgentServiceBackendError> {
        verify_cursor_identity_at(
            &self.runtime.dashboard_origin,
            credentials,
            self.config.expected_user_id,
            self.config.expected_team_id,
        )
        .await?;
        let mut transport = AgentServiceTransport::connect(&self.runtime.service_origin).await?;
        Ok(transport.start_run(request.clone(), credentials).await?)
    }

    #[cfg(test)]
    fn new_for_test(
        config: CursorAgentServiceBackendConfig,
        credential_store_path: PathBuf,
        dashboard_origin: String,
        service_origin: String,
    ) -> Self {
        Self {
            config,
            runtime: BackendRuntime {
                credential_source: CredentialSource::Explicit(credential_store_path),
                dashboard_origin,
                service_origin,
            },
            auth_retry_lock: Mutex::new(()),
        }
    }
}

#[derive(Debug)]
struct BackendRuntime {
    credential_source: CredentialSource,
    dashboard_origin: String,
    service_origin: String,
}

impl BackendRuntime {
    fn credential_store(&self) -> Result<CursorCredentialStore, CursorCredentialStoreError> {
        match &self.credential_source {
            CredentialSource::Environment => CursorCredentialStore::from_environment(),
            #[cfg(test)]
            CredentialSource::Explicit(path) => Ok(CursorCredentialStore::new(path.clone())),
        }
    }
}

#[derive(Debug)]
enum CredentialSource {
    Environment,
    #[cfg(test)]
    Explicit(PathBuf),
}

#[derive(Debug, Error)]
pub enum CursorAgentServiceBackendError {
    #[error(transparent)]
    Mapping(#[from] CursorMappingError),
    #[error(transparent)]
    CredentialStore(#[from] CursorCredentialStoreError),
    #[error(transparent)]
    Identity(#[from] CursorIdentityError),
    #[error(transparent)]
    Transport(#[from] AgentServiceTransportError),
    #[error(transparent)]
    Session(#[from] CursorAgentServiceSessionError),
    #[error(
        "Cursor AgentService origin drift: expected {expected}, configured {actual}"
    )]
    UnexpectedServiceOrigin {
        expected: &'static str,
        actual: String,
    },
}

impl CursorAgentServiceBackendError {
    fn is_unauthenticated(&self) -> bool {
        match self {
            Self::Identity(error) => error.is_unauthenticated(),
            Self::Transport(error) => error.is_unauthenticated(),
            Self::Mapping(_)
            | Self::CredentialStore(_)
            | Self::Session(_)
            | Self::UnexpectedServiceOrigin { .. } => false,
        }
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
