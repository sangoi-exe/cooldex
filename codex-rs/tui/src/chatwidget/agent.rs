use std::sync::Arc;

use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TokenUsageInfo;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

const TUI_NOTIFY_CLIENT: &str = "codex-tui";

async fn initialize_app_server_client_name(thread: &CodexThread) {
    if let Err(err) = thread
        .set_app_server_client_name(Some(TUI_NOTIFY_CLIENT.to_string()))
        .await
    {
        tracing::error!("failed to set app server client name: {err}");
    }
}

fn prompt_gc_activity_event(thread_id: Option<codex_protocol::ThreadId>, active: bool) -> AppEvent {
    match thread_id {
        Some(thread_id) => AppEvent::ThreadPromptGcActivity { thread_id, active },
        None => AppEvent::PromptGcActivity { active },
    }
}

fn prompt_gc_context_usage_event(
    thread_id: Option<codex_protocol::ThreadId>,
    token_usage_info: Option<TokenUsageInfo>,
) -> AppEvent {
    match thread_id {
        Some(thread_id) => AppEvent::ThreadPromptGcContextUsageUpdated {
            thread_id,
            token_usage_info,
        },
        None => AppEvent::PromptGcContextUsageUpdated { token_usage_info },
    }
}

fn initial_prompt_gc_bootstrap_event(
    thread_id: Option<codex_protocol::ThreadId>,
    prompt_gc_active: bool,
    seed_prompt_gc_context_usage_if_idle: bool,
    token_usage_info: Option<TokenUsageInfo>,
) -> Option<AppEvent> {
    if prompt_gc_active {
        Some(prompt_gc_activity_event(thread_id, true))
    } else if seed_prompt_gc_context_usage_if_idle {
        Some(prompt_gc_context_usage_event(thread_id, token_usage_info))
    } else {
        None
    }
}

// Merge-safety anchor: prompt-GC-private idle bootstrap must follow the same lead-session
// eligibility rule as the runtime so child threads do not look prompt-GC-capable from token usage.
fn allow_idle_prompt_gc_bootstrap(
    session_source: &SessionSource,
    seed_prompt_gc_context_usage_if_idle: bool,
) -> bool {
    seed_prompt_gc_context_usage_if_idle && !matches!(session_source, SessionSource::SubAgent(_))
}

// Merge-safety anchor: prompt-GC completion usage refresh must stay on the private TUI runtime
// path so hidden prompt-GC can refresh context-left without surfacing a protocol TokenCount.
async fn emit_prompt_gc_context_usage_update(
    thread: &CodexThread,
    app_event_tx: &AppEventSender,
    thread_id: Option<codex_protocol::ThreadId>,
) {
    let token_usage_info = thread.token_usage_info().await;
    app_event_tx.send(prompt_gc_context_usage_event(thread_id, token_usage_info));
}

pub(crate) async fn forward_thread_runtime(
    thread: Arc<CodexThread>,
    app_event_tx: AppEventSender,
    thread_id: Option<codex_protocol::ThreadId>,
    seed_prompt_gc_context_usage_if_idle: bool,
) {
    let prompt_gc_state_rx = thread.subscribe_prompt_gc_activity();
    let mut prompt_gc_edge_rx = thread.subscribe_prompt_gc_activity_edges();
    let prompt_gc_active = *prompt_gc_state_rx.borrow();
    let idle_prompt_gc_bootstrap_allowed = if seed_prompt_gc_context_usage_if_idle {
        let session_source = thread.config_snapshot().await.session_source;
        allow_idle_prompt_gc_bootstrap(&session_source, seed_prompt_gc_context_usage_if_idle)
    } else {
        false
    };
    let initial_token_usage_info = if !prompt_gc_active && idle_prompt_gc_bootstrap_allowed {
        thread.token_usage_info().await
    } else {
        None
    };
    if let Some(initial_event) = initial_prompt_gc_bootstrap_event(
        thread_id,
        prompt_gc_active,
        idle_prompt_gc_bootstrap_allowed,
        initial_token_usage_info,
    ) {
        app_event_tx.send(initial_event);
    }

    loop {
        tokio::select! {
            event = thread.next_event() => {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::debug!("thread runtime listener stopped: {err}");
                        break;
                    }
                };
                let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
                match thread_id {
                    Some(thread_id) => app_event_tx.send(AppEvent::ThreadEvent { thread_id, event }),
                    None => app_event_tx.send(AppEvent::CodexEvent(event)),
                }
                if is_shutdown_complete {
                    break;
                }
            }
            edge = prompt_gc_edge_rx.recv() => {
                match edge {
                    Ok(active) => {
                        app_event_tx.send(prompt_gc_activity_event(thread_id, active));
                        if !active {
                            emit_prompt_gc_context_usage_update(
                                thread.as_ref(),
                                &app_event_tx,
                                thread_id,
                            )
                            .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let active = *prompt_gc_state_rx.borrow();
                        app_event_tx.send(prompt_gc_activity_event(thread_id, active));
                        if !active {
                            emit_prompt_gc_context_usage_update(
                                thread.as_ref(),
                                &app_event_tx,
                                thread_id,
                            )
                            .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    if *prompt_gc_state_rx.borrow() {
        app_event_tx.send(prompt_gc_activity_event(thread_id, false));
        emit_prompt_gc_context_usage_update(thread.as_ref(), &app_event_tx, thread_id).await;
    }
}

/// Spawn the agent bootstrapper and op forwarding loop, returning the
/// `UnboundedSender<Op>` used by the UI to submit operations.
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<ThreadManager>,
) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        let NewThread {
            thread,
            session_configured,
            ..
        } = match server.start_thread(config).await {
            Ok(v) => v,
            Err(err) => {
                let message = format!("Failed to initialize codex: {err}");
                tracing::error!("{message}");
                app_event_tx_clone.send(AppEvent::CodexEvent(Event {
                    id: "".to_string(),
                    msg: EventMsg::Error(err.to_error_event(None)),
                }));
                app_event_tx_clone.send(AppEvent::FatalExitRequest(message));
                tracing::error!("failed to initialize codex: {err}");
                return;
            }
        };
        initialize_app_server_client_name(thread.as_ref()).await;

        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = codex_protocol::protocol::Event {
            // The `id` does not matter for rendering, so we can use a fake value.
            id: "".to_string(),
            msg: codex_protocol::protocol::EventMsg::SessionConfigured(session_configured),
        };
        app_event_tx_clone.send(AppEvent::CodexEvent(ev));

        let thread_clone = thread.clone();
        tokio::spawn(async move {
            while let Some(op) = codex_op_rx.recv().await {
                let id = thread_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        forward_thread_runtime(thread, app_event_tx_clone, None, true).await;
    });

    codex_op_tx
}

/// Spawn agent loops for an existing thread (e.g., a forked thread).
/// Sends the provided `SessionConfiguredEvent` immediately, then forwards subsequent
/// events and accepts Ops for submission.
pub(crate) fn spawn_agent_from_existing(
    thread: std::sync::Arc<CodexThread>,
    session_configured: codex_protocol::protocol::SessionConfiguredEvent,
    app_event_tx: AppEventSender,
) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        initialize_app_server_client_name(thread.as_ref()).await;

        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = codex_protocol::protocol::Event {
            id: "".to_string(),
            msg: codex_protocol::protocol::EventMsg::SessionConfigured(session_configured),
        };
        app_event_tx_clone.send(AppEvent::CodexEvent(ev));

        let thread_clone = thread.clone();
        tokio::spawn(async move {
            while let Some(op) = codex_op_rx.recv().await {
                let id = thread_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        forward_thread_runtime(thread, app_event_tx_clone, None, true).await;
    });

    codex_op_tx
}

/// Spawn an op-forwarding loop for an existing thread without subscribing to events.
pub(crate) fn spawn_op_forwarder(thread: std::sync::Arc<CodexThread>) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    tokio::spawn(async move {
        initialize_app_server_client_name(thread.as_ref()).await;
        while let Some(op) = codex_op_rx.recv().await {
            if let Err(e) = thread.submit(op).await {
                tracing::error!("failed to submit op: {e}");
            }
        }
    });

    codex_op_tx
}

#[cfg(test)]
mod tests {
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::TokenUsage;

    use super::*;

    #[test]
    fn initial_prompt_gc_bootstrap_event_emits_thread_usage_refresh_when_listener_attaches_idle() {
        let thread_id = ThreadId::new();
        let token_usage_info = TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 950_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_400,
                ..TokenUsage::default()
            },
            model_context_window: Some(13_000),
        };

        let event = initial_prompt_gc_bootstrap_event(
            Some(thread_id),
            false,
            true,
            Some(token_usage_info.clone()),
        )
        .expect("idle bootstrap should emit a private usage refresh");

        match event {
            AppEvent::ThreadPromptGcContextUsageUpdated {
                thread_id: event_thread_id,
                token_usage_info: event_token_usage_info,
            } => {
                assert_eq!(event_thread_id, thread_id);
                assert_eq!(event_token_usage_info, Some(token_usage_info));
            }
            other => panic!("expected thread prompt-GC usage refresh, got {other:?}"),
        }
    }

    #[test]
    fn initial_prompt_gc_bootstrap_event_emits_primary_usage_refresh_when_listener_attaches_idle() {
        let token_usage_info = TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 950_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_400,
                ..TokenUsage::default()
            },
            model_context_window: Some(13_000),
        };

        let event =
            initial_prompt_gc_bootstrap_event(None, false, true, Some(token_usage_info.clone()))
                .expect("idle bootstrap should emit a primary private usage refresh");

        match event {
            AppEvent::PromptGcContextUsageUpdated {
                token_usage_info: event_token_usage_info,
            } => {
                assert_eq!(event_token_usage_info, Some(token_usage_info));
            }
            other => panic!("expected primary prompt-GC usage refresh, got {other:?}"),
        }
    }

    #[test]
    fn idle_prompt_gc_bootstrap_is_disabled_for_subagent_sessions() {
        let session_source = SessionSource::SubAgent(
            codex_protocol::protocol::SubAgentSource::Other("bug-hunter".to_string()),
        );

        assert!(!allow_idle_prompt_gc_bootstrap(&session_source, true));
    }

    #[test]
    fn idle_prompt_gc_bootstrap_stays_enabled_for_lead_sessions() {
        assert!(allow_idle_prompt_gc_bootstrap(&SessionSource::Cli, true));
        assert!(!allow_idle_prompt_gc_bootstrap(&SessionSource::Cli, false));
    }
}
