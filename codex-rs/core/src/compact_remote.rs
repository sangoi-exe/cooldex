use std::sync::Arc;

use crate::Prompt;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex::maybe_auto_switch_account_on_usage_limit;
use crate::compact::InitialContextInjection;
use crate::compact::insert_initial_context_before_last_real_user_or_summary;
use crate::context_manager::ContextManager;
use crate::context_manager::TotalTokenUsageBreakdown;
use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::context_manager::is_codex_generated_item;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::protocol::CompactedItem;
use crate::protocol::EventMsg;
use crate::protocol::TurnStartedEvent;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;
use tracing::error;
use tracing::info;

pub(crate) async fn run_inline_remote_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> CodexResult<()> {
    run_remote_compact_task_inner(&sess, &turn_context, initial_context_injection).await?;
    Ok(())
}

pub(crate) async fn run_remote_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) -> CodexResult<()> {
    let start_event = EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_context.sub_id.clone(),
        model_context_window: turn_context.model_context_window(),
        collaboration_mode_kind: turn_context.collaboration_mode.mode,
    });
    sess.send_event(&turn_context, start_event).await;

    run_remote_compact_task_inner(&sess, &turn_context, InitialContextInjection::DoNotInject).await
}

async fn run_remote_compact_task_inner(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> CodexResult<()> {
    if let Err(err) =
        run_remote_compact_task_inner_impl(sess, turn_context, initial_context_injection).await
    {
        let event = EventMsg::Error(
            err.to_error_event(Some("Error running remote compact task".to_string())),
        );
        sess.send_event(turn_context, event).await;
        return Err(err);
    }
    Ok(())
}

async fn run_remote_compact_task_inner_impl(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> CodexResult<()> {
    let compaction_item = TurnItem::ContextCompaction(ContextCompactionItem::new());
    sess.emit_turn_item_started(turn_context, &compaction_item)
        .await;
    let mut history = sess.clone_history().await;
    // Keep compaction prompts in-distribution: if a model-switch update was injected at the
    // tail of history (between turns), exclude it from the compaction request payload.
    let stripped_model_switch_item =
        extract_trailing_model_switch_update_for_compaction_request(&mut history);

    let base_instructions = sess.get_base_instructions().await;
    let deleted_items = trim_function_call_history_to_fit_context_window(
        &mut history,
        turn_context.as_ref(),
        &base_instructions,
    );
    if deleted_items > 0 {
        info!(
            turn_id = %turn_context.sub_id,
            deleted_items,
            "trimmed history items before remote compaction"
        );
    }
    // Required to keep `/undo` available after compaction
    let ghost_snapshots: Vec<ResponseItem> = history
        .raw_items()
        .iter()
        .filter(|item| matches!(item, ResponseItem::GhostSnapshot { .. }))
        .cloned()
        .collect();
    let prompt = Prompt {
        input: history.for_prompt(&turn_context.model_info.input_modalities),
        tools: vec![],
        parallel_tool_calls: false,
        base_instructions,
        personality: turn_context.personality,
        output_schema: None,
    };

    let compact_request_log_data =
        build_compact_request_log_data(&prompt.input, &prompt.base_instructions.text);
    let mut new_history = loop {
        let request_store_account_id = sess
            .services
            .auth_manager
            .auth_cached()
            .as_ref()
            .and_then(|auth| auth.get_account_id());
        match sess
            .services
            .model_client
            .compact_conversation_history(
                &prompt,
                &turn_context.model_info,
                &turn_context.otel_manager,
            )
            .await
        {
            Ok(new_history) => break new_history,
            Err(CodexErr::UsageLimitReached(usage_limit)) => {
                let rate_limits = usage_limit.rate_limits.clone();
                if let Some(rate_limits) = rate_limits {
                    sess.update_rate_limits(turn_context, *rate_limits).await;
                }

                if maybe_auto_switch_account_on_usage_limit(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &usage_limit,
                    request_store_account_id.as_deref(),
                )
                .await?
                {
                    continue;
                }

                let err = CodexErr::UsageLimitReached(usage_limit);
                let total_usage_breakdown = sess.get_total_token_usage_breakdown().await;
                log_remote_compact_failure(
                    turn_context,
                    &compact_request_log_data,
                    total_usage_breakdown,
                    &err,
                );
                return Err(err);
            }
            Err(err) => {
                let total_usage_breakdown = sess.get_total_token_usage_breakdown().await;
                log_remote_compact_failure(
                    turn_context,
                    &compact_request_log_data,
                    total_usage_breakdown,
                    &err,
                );
                return Err(err);
            }
        }
    };
    new_history = process_compacted_history(
        sess.as_ref(),
        turn_context.as_ref(),
        new_history,
        initial_context_injection,
    )
    .await;

    // Reattach the stripped model-switch update only after successful compaction so the model
    // still sees the switch instructions on the next real sampling request.
    if let Some(model_switch_item) = stripped_model_switch_item {
        new_history.push(model_switch_item);
    }

    if !ghost_snapshots.is_empty() {
        new_history.extend(ghost_snapshots);
    }
    let reference_context_item = match initial_context_injection {
        InitialContextInjection::DoNotInject => None,
        InitialContextInjection::BeforeLastUserMessage => Some(turn_context.to_turn_context_item()),
    };
    let compacted_item = CompactedItem {
        message: String::new(),
        replacement_history: Some(new_history.clone()),
    };
    sess.replace_compacted_history(new_history, reference_context_item, compacted_item)
        .await;
    sess.recompute_token_usage(turn_context).await;

    sess.emit_turn_item_completed(turn_context, compaction_item)
        .await;
    Ok(())
}

fn extract_trailing_model_switch_update_for_compaction_request(
    history: &mut ContextManager,
) -> Option<ResponseItem> {
    let trailing_item = history.raw_items().last()?.clone();
    if !is_model_switch_update_item(&trailing_item) {
        return None;
    }

    if !history.remove_last_item() {
        return None;
    }

    Some(trailing_item)
}

fn is_model_switch_update_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, content, .. } if role == "developer" => {
            content.iter().any(|content_item| {
                matches!(
                    content_item,
                    codex_protocol::models::ContentItem::InputText { text }
                        if text.contains("<model_switch>")
                )
            })
        }
        _ => false,
    }
}

pub(crate) async fn process_compacted_history(
    sess: &Session,
    turn_context: &TurnContext,
    mut compacted_history: Vec<ResponseItem>,
    initial_context_injection: InitialContextInjection,
) -> Vec<ResponseItem> {
    // Mid-turn compaction is the only path that must inject initial context above the last user
    // message in the replacement history. Pre-turn compaction instead injects context after the
    // compaction item, but mid-turn compaction keeps the compaction item last for model training.
    let initial_context = if matches!(
        initial_context_injection,
        InitialContextInjection::BeforeLastUserMessage
    ) {
        sess.build_initial_context(turn_context).await
    } else {
        Vec::new()
    };

    compacted_history.retain(should_keep_compacted_history_item);
    insert_initial_context_before_last_real_user_or_summary(compacted_history, initial_context)
}

/// Returns whether an item from remote compaction output should be preserved.
///
/// Called while processing the model-provided compacted transcript, before we
/// append fresh canonical context from the current session.
///
/// We drop:
/// - `developer` messages because remote output can include stale/duplicated
///   instruction content.
/// - non-user-content `user` messages (session prefix/instruction wrappers),
///   keeping only real user messages as parsed by `parse_turn_item`.
///
/// This intentionally keeps:
/// - `assistant` messages (future remote compaction models may emit them)
/// - `user`-role warnings and compaction-generated summary messages because
///   they parse as `TurnItem::UserMessage`.
fn should_keep_compacted_history_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, .. } if role == "developer" => false,
        ResponseItem::Message { role, .. } if role == "user" => {
            matches!(
                crate::event_mapping::parse_turn_item(item),
                Some(TurnItem::UserMessage(_))
            )
        }
        ResponseItem::Message { role, .. } if role == "assistant" => true,
        ResponseItem::Message { .. } => false,
        ResponseItem::Compaction { .. } => true,
        ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Other => false,
    }
}

#[derive(Debug)]
struct CompactRequestLogData {
    failing_compaction_request_model_visible_bytes: i64,
}

fn build_compact_request_log_data(
    input: &[ResponseItem],
    instructions: &str,
) -> CompactRequestLogData {
    let failing_compaction_request_model_visible_bytes = input
        .iter()
        .map(estimate_response_item_model_visible_bytes)
        .fold(
            i64::try_from(instructions.len()).unwrap_or(i64::MAX),
            i64::saturating_add,
        );

    CompactRequestLogData {
        failing_compaction_request_model_visible_bytes,
    }
}

fn log_remote_compact_failure(
    turn_context: &TurnContext,
    log_data: &CompactRequestLogData,
    total_usage_breakdown: TotalTokenUsageBreakdown,
    err: &CodexErr,
) {
    error!(
        turn_id = %turn_context.sub_id,
        last_api_response_total_tokens = total_usage_breakdown.last_api_response_total_tokens,
        all_history_items_model_visible_bytes = total_usage_breakdown.all_history_items_model_visible_bytes,
        estimated_tokens_of_items_added_since_last_successful_api_response = total_usage_breakdown.estimated_tokens_of_items_added_since_last_successful_api_response,
        estimated_bytes_of_items_added_since_last_successful_api_response = total_usage_breakdown.estimated_bytes_of_items_added_since_last_successful_api_response,
        model_context_window_tokens = ?turn_context.model_context_window(),
        failing_compaction_request_model_visible_bytes = log_data.failing_compaction_request_model_visible_bytes,
        compact_error = %err,
        "remote compaction failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use chrono::Utc;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use crate::auth::AuthCredentialsStoreMode;
    use crate::auth::AuthManager;
    use crate::auth::AuthStore;
    use crate::auth::CodexAuth;
    use crate::auth::StoredAccount;
    use crate::auth::save_auth;
    use crate::client::ModelClient;
    use crate::codex::make_session_and_context_with_rx;
    use crate::features::Feature;
    use codex_protocol::models::ContentItem;
    use codex_protocol::protocol::Event;
    use codex_protocol::protocol::RateLimitSnapshot;
    use codex_protocol::protocol::RateLimitWindow;

    struct CompactRetryResponder {
        num_calls: AtomicUsize,
        resets_at: i64,
    }

    impl Respond for CompactRetryResponder {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            match self.num_calls.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(429)
                    .insert_header("content-type", "application/json")
                    .insert_header("x-codex-primary-used-percent", "100.0")
                    .insert_header("x-codex-secondary-used-percent", "87.5")
                    .insert_header("x-codex-primary-over-secondary-limit-percent", "95.0")
                    .insert_header("x-codex-primary-window-minutes", "15")
                    .insert_header("x-codex-secondary-window-minutes", "60")
                    .set_body_json(json!({
                        "error": {
                            "type": "usage_limit_reached",
                            "plan_type": "pro",
                            "resets_at": self.resets_at,
                        }
                    })),
                1 => ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(json!({
                        "output": [
                            {
                                "type": "compaction",
                                "encrypted_content": "remote compact summary",
                            }
                        ]
                    })),
                call_num => panic!("unexpected compact request {call_num}"),
            }
        }
    }

    fn token_data_for_account(account_id: &str) -> crate::token_data::TokenData {
        #[derive(serde::Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = json!({
            "email": format!("{account_id}@example.com"),
            "email_verified": true,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_user_id": "user-12345",
                "user_id": "user-12345",
                "chatgpt_account_id": account_id,
            }
        });
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let header_b64 = encode(&serde_json::to_vec(&header).expect("serialize jwt header"));
        let payload_b64 = encode(&serde_json::to_vec(&payload).expect("serialize jwt payload"));
        let signature_b64 = encode(b"sig");
        let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

        crate::token_data::TokenData {
            id_token: crate::token_data::parse_chatgpt_jwt_claims(&fake_jwt)
                .expect("parse fake chatgpt jwt"),
            access_token: format!("access-{account_id}"),
            refresh_token: format!("refresh-{account_id}"),
            account_id: Some(account_id.to_string()),
        }
    }

    fn stored_account(account_id: &str, label: &str) -> StoredAccount {
        StoredAccount {
            id: account_id.to_string(),
            label: Some(label.to_string()),
            tokens: token_data_for_account(account_id),
            last_refresh: Some(Utc::now()),
            usage: None,
        }
    }

    fn drain_warning_messages(rx_event: &async_channel::Receiver<Event>) -> Vec<String> {
        let mut warnings = Vec::new();
        while let Ok(event) = rx_event.try_recv() {
            if let EventMsg::Warning(warning) = event.msg {
                warnings.push(warning.message);
            }
        }
        warnings
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_compact_usage_limit_auto_switches_and_retries() {
        let server = MockServer::start().await;
        let resets_at = (Utc::now() + chrono::Duration::minutes(60)).timestamp();

        Mock::given(method("POST"))
            .and(path("/v1/responses/compact"))
            .respond_with(CompactRetryResponder {
                num_calls: AtomicUsize::new(0),
                resets_at,
            })
            .expect(2)
            .mount(&server)
            .await;

        let expected_rate_limits = RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 100.0,
                window_minutes: Some(15),
                resets_at: None,
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 87.5,
                window_minutes: Some(60),
                resets_at: None,
            }),
            credits: None,
            plan_type: None,
        };

        let (mut session, mut turn_context, rx_event) = make_session_and_context_with_rx().await;

        let auth_home = TempDir::new().expect("create auth tempdir");
        let auth_store = AuthStore {
            active_account_id: Some("acc-1".to_string()),
            accounts: vec![
                stored_account("acc-1", "primary"),
                stored_account("acc-2", "secondary"),
            ],
            ..AuthStore::default()
        };
        save_auth(
            auth_home.path(),
            &auth_store,
            AuthCredentialsStoreMode::File,
        )
        .expect("persist multi-account auth store");
        let auth_manager = AuthManager::shared(
            auth_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        );
        assert_eq!(
            auth_manager.auth_mode(),
            Some(crate::auth::AuthMode::Chatgpt),
            "fixture auth manager should load as ChatGPT auth"
        );
        assert_eq!(
            auth_manager
                .auth_cached()
                .as_ref()
                .and_then(CodexAuth::get_account_id)
                .as_deref(),
            Some("acc-1"),
            "fixture auth manager should expose acc-1 as the active cached auth"
        );
        assert_eq!(
            auth_manager.list_accounts().len(),
            2,
            "fixture auth manager should expose both stored accounts"
        );
        assert_eq!(
            auth_manager
                .select_account_for_auto_switch(None, Some("acc-1"))
                .as_deref(),
            Some("acc-2"),
            "fixture auth manager should select acc-2 as the auto-switch target"
        );

        let mut provider = crate::built_in_model_providers()["openai"].clone();
        provider.base_url = Some(format!("{}/v1", server.uri()));

        let session_mut = Arc::get_mut(&mut session).expect("session arc should be unique");
        session_mut.services.auth_manager = Arc::clone(&auth_manager);
        session_mut.services.model_client = ModelClient::new(
            Some(Arc::clone(&auth_manager)),
            session_mut.conversation_id,
            provider,
            turn_context.session_source.clone(),
            turn_context.config.model_verbosity,
            crate::ws_version_from_features(turn_context.config.as_ref()),
            turn_context
                .config
                .features
                .enabled(Feature::EnableRequestCompression),
            turn_context
                .config
                .features
                .enabled(Feature::RuntimeMetrics),
            None,
        );

        let turn_context_mut =
            Arc::get_mut(&mut turn_context).expect("turn context arc should be unique");
        turn_context_mut.auth_manager = Some(Arc::clone(&auth_manager));

        let history_item = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "seed compact history".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        session
            .record_into_history(std::slice::from_ref(&history_item), turn_context.as_ref())
            .await;

        run_remote_compact_task_inner_impl(
            &session,
            &turn_context,
            InitialContextInjection::DoNotInject,
        )
        .await
        .expect("remote compact should retry after auto-switch");

        let warning_messages = drain_warning_messages(&rx_event);
        assert_eq!(
            warning_messages,
            vec![
                "Usage limit reached for account 'primary'. Auto-switched to account 'secondary' and retrying."
                    .to_string()
            ]
        );

        let accounts = auth_manager.list_accounts();
        assert_eq!(
            accounts
                .iter()
                .find(|account| account.is_active)
                .map(|account| account.id.as_str()),
            Some("acc-2")
        );
        assert!(
            accounts
                .iter()
                .find(|account| account.id == "acc-1")
                .and_then(|account| account.exhausted_until)
                .is_some(),
            "failing account should be marked exhausted after auto-switch"
        );

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(requests.len(), 2, "expected one retry after auto-switch");
        assert_eq!(
            requests[0]
                .headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
            Some("acc-1")
        );
        assert_eq!(
            requests[1]
                .headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
            Some("acc-2")
        );
        let latest_rate_limits = session.state.lock().await.latest_rate_limits.clone();
        assert_eq!(latest_rate_limits, Some(expected_rate_limits));
        assert!(
            session
                .clone_history()
                .await
                .raw_items()
                .iter()
                .any(|item| matches!(item, ResponseItem::Compaction { .. })),
            "successful retry should persist compacted history"
        );

        while let Ok(event) = rx_event.try_recv() {
            assert!(
                !matches!(event.msg, EventMsg::Error(_)),
                "remote compact retry should not emit an error event"
            );
        }

        server.verify().await;
    }
}

fn trim_function_call_history_to_fit_context_window(
    history: &mut ContextManager,
    turn_context: &TurnContext,
    base_instructions: &BaseInstructions,
) -> usize {
    let mut deleted_items = 0usize;
    let Some(context_window) = turn_context.model_context_window() else {
        return deleted_items;
    };

    while history
        .estimate_token_count_with_base_instructions(base_instructions)
        .is_some_and(|estimated_tokens| estimated_tokens > context_window)
    {
        let Some(last_item) = history.raw_items().last() else {
            break;
        };
        if !is_codex_generated_item(last_item) {
            break;
        }
        if !history.remove_last_item() {
            break;
        }
        deleted_items += 1;
    }

    deleted_items
}
