//! Test-only helpers exposed for cross-crate integration tests.
//!
//! Production code should not depend on this module.
//! We prefer this to using a crate feature to avoid building multiple
//! permutations of the crate.

use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex as StdMutex;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::time::Duration;

use codex_exec_server::EnvironmentManager;
#[cfg(test)]
use codex_extension_api::Instructions;
use codex_extension_api::LoadInstructionsFuture;
use codex_extension_api::LoadedUserInstructions;
#[cfg(test)]
use codex_extension_api::ThreadInstructionsProvider;
use codex_extension_api::UserInstructionsProvider;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
#[cfg(test)]
use codex_models_manager::ModelsManagerConfig;
use codex_models_manager::bundled_models_response;
use codex_models_manager::collaboration_mode_presets;
#[cfg(test)]
use codex_models_manager::manager::ModelsManager;
#[cfg(test)]
use codex_models_manager::manager::ModelsManagerFuture;
#[cfg(test)]
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
#[cfg(test)]
use codex_models_manager::manager::StaticModelsManager;
use codex_models_manager::test_support::construct_model_info_offline_for_tests;
use codex_models_manager::test_support::get_model_offline_for_tests;
use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::mcp::OPENAI_FORM_EXTENSION_ID;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
#[cfg(test)]
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::SessionSource;
use once_cell::sync::Lazy;
#[cfg(test)]
use tokio::sync::Notify;
#[cfg(test)]
use tokio::sync::TryLockError;
#[cfg(test)]
use tokio::time::timeout;

use crate::ThreadManager;
use crate::config::Config;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_metadata::subagent_header_value;
use crate::responses_metadata::subagent_metadata_kind;
use crate::thread_manager;
use crate::unified_exec;

#[cfg(test)]
pub(crate) struct GatedInstructionsProvider {
    pub(crate) instructions: StdMutex<Option<Instructions>>,
    pub(crate) block_next_read: AtomicBool,
    pub(crate) read_started: Notify,
}

#[cfg(test)]
impl GatedInstructionsProvider {
    fn load(&self) -> LoadInstructionsFuture<'_> {
        let instructions = self
            .instructions
            .lock()
            .expect("instruction snapshot")
            .clone();
        let block = self.block_next_read.swap(/*val*/ false, Ordering::SeqCst);
        Box::pin(async move {
            if block {
                self.read_started.notify_one();
                std::future::pending::<()>().await;
            }
            LoadedUserInstructions {
                instructions,
                warnings: Vec::new(),
            }
        })
    }
}

#[cfg(test)]
impl UserInstructionsProvider for GatedInstructionsProvider {
    fn load_user_instructions(&self) -> LoadInstructionsFuture<'_> {
        self.load()
    }
}

#[cfg(test)]
impl ThreadInstructionsProvider for GatedInstructionsProvider {
    fn load_thread_instructions(&self) -> LoadInstructionsFuture<'_> {
        self.load()
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ModelLookupGate {
    started: Notify,
    resume: Notify,
}

#[cfg(test)]
impl ModelLookupGate {
    pub(crate) async fn wait_until_blocked(&self) {
        timeout(Duration::from_secs(/*secs*/ 10), self.started.notified())
            .await
            .expect("model lookup started");
    }

    pub(crate) fn release(&self) {
        self.resume.notify_one();
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct GatedModelsManager {
    inner: StaticModelsManager,
    gated_model: String,
    first_lookup: StdMutex<Option<Arc<ModelLookupGate>>>,
}

#[cfg(test)]
impl GatedModelsManager {
    pub(crate) fn new(
        models: Vec<ModelInfo>,
        gated_model: &str,
    ) -> (Arc<Self>, Arc<ModelLookupGate>) {
        let lookup = Arc::new(ModelLookupGate::default());
        (
            Arc::new(Self {
                inner: StaticModelsManager::new(
                    /*auth_manager*/ None,
                    ModelsResponse { models },
                    codex_models_manager::ModelOrigin::ExplicitConfig,
                ),
                gated_model: gated_model.to_owned(),
                first_lookup: StdMutex::new(Some(Arc::clone(&lookup))),
            }),
            lookup,
        )
    }
}

#[cfg(test)]
impl ModelsManager for GatedModelsManager {
    fn model_catalog_with_origins(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<codex_models_manager::CatalogModel>> {
        self.inner.model_catalog_with_origins(strategy, factory)
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        self.inner.get_remote_models()
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        self.inner.try_get_remote_models()
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        self.inner.auth_manager()
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.inner.list_collaboration_modes()
    }

    fn refresh_if_new_etag(
        &self,
        etag: String,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        self.inner.refresh_if_new_etag(etag, factory)
    }

    fn get_model_info<'a>(
        &'a self,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, ModelInfo> {
        Box::pin(async move {
            let pause = if model == self.gated_model.as_str() {
                self.first_lookup.lock().expect("lookup gate").take()
            } else {
                None
            };
            if let Some(pause) = pause {
                pause.started.notify_one();
                pause.resume.notified().await;
            }
            self.inner.get_model_info(model, config).await
        })
    }
}

static TEST_MODEL_PRESETS: Lazy<Vec<ModelPreset>> = Lazy::new(|| {
    let mut response = bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    response.models.sort_by_key(|model| model.priority);
    let mut presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
});

/// Inspect the same resolved environment configurations used to construct sampling steps.
pub async fn environment_windows_sandbox_types(
    thread: &crate::CodexThread,
) -> Vec<(String, codex_sandboxing::SandboxType)> {
    thread
        .session
        .services
        .turn_environments
        .snapshot()
        .await
        .turn_environments()
        .map(|environment| {
            (
                environment.selection.environment_id.clone(),
                environment.config().windows_sandbox_type,
            )
        })
        .collect()
}

/// Reattaches request-only observations to a completed turn's history for capture assertions.
/// Tests inspect this separately from the destination-filtered HTTP/WS request.
pub async fn history_with_tool_call_metadata(
    thread: &crate::CodexThread,
) -> Vec<codex_protocol::models::ResponseItem> {
    let history = thread.conversation_history_snapshot().await;
    let mut items = history.items().cloned().collect::<Vec<_>>();
    thread
        .session
        .services
        .executed_tool_calls
        .attach_to_prompt(&mut items, &mut Default::default());
    items
}

/// Returns the recorder state used by the next session metadata snapshot.
/// This does not change destination filtering or issue a request.
pub fn mcp_attribution_snapshot(
    thread: &crate::CodexThread,
) -> codex_protocol::mcp::McpAttribution {
    thread
        .session
        .services
        .executed_tool_calls
        .mcp_attribution_snapshot()
}

/// Retains the active turn's timing after completion without exporting telemetry.
/// Returns `None` when there is no running task. The observer reports exclusive
/// durations for the caller's interval in inference, automatic review, local
/// tools, overhead, and unknown order.
pub async fn observe_turn_timing(
    thread: &crate::CodexThread,
) -> Option<
    impl Fn(std::time::Instant, std::time::Instant) -> Option<[std::time::Duration; 5]> + use<>,
> {
    let active = thread.session.active_turn.lock().await;
    let timing = Arc::clone(
        &active
            .as_ref()?
            .task
            .as_ref()?
            .turn_context
            .turn_timing_state,
    );
    Some(move |start, end| {
        timing.snapshot_path(end).map(|path| {
            let totals = path.durations(start, end);
            [
                totals.inference,
                totals.auto_review,
                totals.tools,
                totals.overhead,
                totals.unknown,
            ]
        })
    })
}

/// Test-only provider that supplies no user instructions.
#[derive(Debug, Default)]
pub struct EmptyUserInstructionsProvider;

impl UserInstructionsProvider for EmptyUserInstructionsProvider {
    fn load_user_instructions(&self) -> LoadInstructionsFuture<'_> {
        Box::pin(async { LoadedUserInstructions::default() })
    }
}

pub fn set_thread_manager_test_mode(enabled: bool) {
    thread_manager::set_thread_manager_test_mode_for_tests(enabled);
}

pub fn set_deterministic_process_ids(enabled: bool) {
    unified_exec::set_deterministic_process_ids_for_tests(enabled);
}

pub fn auth_manager_from_auth(auth: CodexAuth) -> Arc<AuthManager> {
    AuthManager::from_auth_for_testing(auth)
}

pub fn auth_manager_from_auth_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<AuthManager> {
    AuthManager::from_auth_for_testing_with_home(auth, codex_home)
}

pub fn with_code_mode_host_program(
    thread_manager: ThreadManager,
    host_program: PathBuf,
    config: &crate::config::Config,
) -> ThreadManager {
    thread_manager.with_code_mode_host_program_for_tests(host_program, config)
}

pub fn thread_manager_with_models_provider(
    auth: CodexAuth,
    provider: ModelProviderInfo,
) -> ThreadManager {
    ThreadManager::with_models_provider_for_tests(auth, provider)
}

pub fn thread_manager_with_models_provider_and_home(
    auth: CodexAuth,
    provider: ModelProviderInfo,
    codex_home: PathBuf,
    environment_manager: Arc<EnvironmentManager>,
) -> ThreadManager {
    ThreadManager::with_models_provider_and_home_for_tests(
        auth,
        provider,
        codex_home,
        environment_manager,
    )
}

pub async fn start_thread_with_user_shell_override(
    thread_manager: &ThreadManager,
    config: Config,
    user_shell_override: crate::shell::Shell,
    supports_openai_form_elicitation: bool,
) -> codex_protocol::error::Result<crate::NewThread> {
    thread_manager
        .start_thread_with_user_shell_override_for_tests(
            config,
            user_shell_override,
            ClientMcpExtensions::new(
                supports_openai_form_elicitation
                    .then(|| (OPENAI_FORM_EXTENSION_ID.to_string(), serde_json::json!({}))),
            ),
        )
        .await
}

pub async fn resume_legacy_thread_from_rollout_with_user_shell_override(
    thread_manager: &ThreadManager,
    config: Config,
    rollout_path: PathBuf,
    auth_manager: Arc<AuthManager>,
    user_shell_override: crate::shell::Shell,
    supports_openai_form_elicitation: bool,
) -> codex_protocol::error::Result<crate::NewThread> {
    thread_manager
        .resume_legacy_thread_from_rollout_with_user_shell_override_for_tests(
            config,
            rollout_path,
            auth_manager,
            user_shell_override,
            ClientMcpExtensions::new(
                supports_openai_form_elicitation
                    .then(|| (OPENAI_FORM_EXTENSION_ID.to_string(), serde_json::json!({}))),
            ),
        )
        .await
}

pub fn models_manager_with_provider(
    codex_home: PathBuf,
    auth_manager: Arc<AuthManager>,
    provider: ModelProviderInfo,
) -> SharedModelsManager {
    let provider = create_model_provider(provider, Some(auth_manager));
    provider.models_manager(codex_home, /*config_model_catalog*/ None)
}

pub fn default_http_client_factory() -> HttpClientFactory {
    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
}

pub fn get_model_offline(model: Option<&str>) -> String {
    get_model_offline_for_tests(model)
}

pub fn construct_model_info_offline(model: &str, config: &Config) -> ModelInfo {
    construct_model_info_offline_for_tests(model, &config.to_models_manager_config())
}

#[derive(Clone, Copy)]
pub enum TestCodexResponsesRequestKind {
    Turn,
    Prewarm,
    WebsocketConnection,
}

#[allow(clippy::too_many_arguments)]
pub fn responses_metadata(
    installation_id: &str,
    session_id: &str,
    thread_id: &str,
    turn_id: Option<&str>,
    window_id: String,
    session_source: &SessionSource,
    parent_thread_id: Option<ThreadId>,
    request_kind: TestCodexResponsesRequestKind,
) -> CodexResponsesMetadata {
    let request_kind = match request_kind {
        TestCodexResponsesRequestKind::Turn => Some(CodexResponsesRequestKind::Turn),
        TestCodexResponsesRequestKind::Prewarm => Some(CodexResponsesRequestKind::Prewarm),
        TestCodexResponsesRequestKind::WebsocketConnection => None,
    };
    CodexResponsesMetadata {
        turn_id: request_kind.and(turn_id.map(ToString::to_string)),
        request_kind,
        parent_thread_id,
        subagent_header: subagent_header_value(session_source),
        subagent_kind: request_kind.and_then(|_| subagent_metadata_kind(session_source)),
        ..CodexResponsesMetadata::new(
            installation_id.to_string(),
            session_id.to_string(),
            thread_id.to_string(),
            window_id,
        )
    }
}

pub fn with_parent_turn(mut metadata: CodexResponsesMetadata, id: &str) -> CodexResponsesMetadata {
    metadata.parent_turn_id = Some(id.to_string());
    metadata
}

pub fn all_model_presets() -> &'static Vec<ModelPreset> {
    &TEST_MODEL_PRESETS
}

pub fn builtin_collaboration_mode_presets() -> Vec<CollaborationModeMask> {
    collaboration_mode_presets::builtin_collaboration_mode_presets()
}
