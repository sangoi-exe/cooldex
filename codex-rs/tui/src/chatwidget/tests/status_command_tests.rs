use super::*;
use assert_matches::assert_matches;

// Merge-safety anchor: these `/status` tests are the compiled proof for the local missing vs
// unavailable vs stale-live-refresh contract; merges must keep them wired through
// `chatwidget/tests.rs` and aligned with `status/card.rs`.

#[tokio::test]
async fn status_command_renders_immediately_and_refreshes_rate_limits_for_chatgpt_auth() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output before refresh request, got {other:?}"),
    };
    assert!(
        !rendered.contains("refreshing limits"),
        "expected /status to avoid transient refresh text in terminal history, got: {rendered}"
    );
    assert!(
        !rendered.contains("refresh requested; run /status again shortly."),
        "expected /status to avoid transient refresh-only notice text, got: {rendered}"
    );
    assert!(
        rendered.contains("data not available yet"),
        "expected empty-cache /status to render stable missing-data text, got: {rendered}"
    );
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected rate-limit refresh request, got {other:?}"),
    };
    pretty_assertions::assert_eq!(request_id, 0);
}

#[tokio::test]
async fn status_command_refresh_updates_cached_limits_for_future_status_outputs() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(_)) => {}
        other => panic!("expected status output before refresh request, got {other:?}"),
    }
    let first_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected rate-limit refresh request, got {other:?}"),
    };

    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 55.0)));
    chat.finish_status_rate_limit_refresh(first_request_id);
    drain_insert_history(&mut rx);

    chat.dispatch_command(SlashCommand::Status);
    let refreshed = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected refreshed status output, got {other:?}"),
    };
    assert!(
        refreshed.contains("55% left"),
        "expected a future /status output to use refreshed cached limits, got: {refreshed}"
    );
}

#[tokio::test]
async fn status_command_renders_immediately_without_rate_limit_refresh() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.dispatch_command(SlashCommand::Status);

    assert_matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_)));
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::RefreshRateLimits { .. })),
        "non-ChatGPT sessions should not request a rate-limit refresh for /status"
    );
}

#[tokio::test]
async fn status_command_uses_catalog_default_reasoning_when_config_empty() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.config.model_reasoning_effort = None;

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output, got {other:?}"),
    };
    assert!(
        rendered.contains("gpt-5.4 (reasoning xhigh, summaries auto)"),
        "expected /status to render the catalog default reasoning effort, got: {rendered}"
    );
}

#[tokio::test]
async fn status_command_renders_instruction_sources_from_thread_session() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.instruction_source_paths = vec![chat.config.cwd.join("AGENTS.md")];

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output, got {other:?}"),
    };
    assert!(
        rendered.contains("Agents.md"),
        "expected /status to render app-server instruction sources, got: {rendered}"
    );
    assert!(
        !rendered.contains("Agents.md  <none>"),
        "expected /status to avoid stale <none> when app-server provided instruction sources, got: {rendered}"
    );
}

#[tokio::test]
async fn status_command_overlapping_refreshes_update_matching_cells_only() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);
    match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(_)) => {}
        other => panic!("expected first status output, got {other:?}"),
    }
    let first_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected first refresh request, got {other:?}"),
    };

    chat.dispatch_command(SlashCommand::Status);
    let second_rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected second status output, got {other:?}"),
    };
    let second_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected second refresh request, got {other:?}"),
    };

    assert_ne!(first_request_id, second_request_id);
    assert!(
        !second_rendered.contains("refreshing limits"),
        "expected /status to avoid transient refresh text in terminal history, got: {second_rendered}"
    );

    chat.finish_status_rate_limit_refresh(first_request_id);
    pretty_assertions::assert_eq!(chat.refreshing_status_outputs.len(), 1);

    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 55.0)));
    chat.finish_status_rate_limit_refresh(second_request_id);
    assert!(chat.refreshing_status_outputs.is_empty());
}

#[tokio::test]
async fn status_command_stale_cell_refreshes_in_place_when_refresh_completes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    let captured_at = chrono::Local::now() - chrono::Duration::minutes(20);
    let stale_display =
        crate::status::rate_limit_snapshot_display(&snapshot(/*percent*/ 92.0), captured_at);
    chat.rate_limit_snapshots_by_limit_id
        .insert("codex".to_string(), stale_display);

    chat.dispatch_command(SlashCommand::Status);

    let cell = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected stale status output before refresh request, got {other:?}"),
    };
    let stale_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        stale_rendered.contains("limits may be stale - refreshing limits in background"),
        "expected stale /status output to declare background refresh while it stays live-updating, got: {stale_rendered}"
    );
    assert!(
        stale_rendered.contains("92% left"),
        "expected stale /status output to render the cached rate-limit rows, got: {stale_rendered}"
    );

    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected refresh request for stale status output, got {other:?}"),
    };

    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 55.0)));
    chat.finish_status_rate_limit_refresh(request_id);

    let refreshed_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        refreshed_rendered.contains("55% left"),
        "expected the already-rendered /status cell to refresh in place when the new data arrives, got: {refreshed_rendered}"
    );
    assert!(
        !refreshed_rendered.contains("limits may be stale"),
        "expected the stale warning to clear after the in-place refresh completed, got: {refreshed_rendered}"
    );
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "expected the refresh to update the existing /status cell instead of inserting a second history cell"
    );
}

#[tokio::test]
async fn status_command_empty_successful_refresh_becomes_unavailable() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    let cell = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected status output before refresh request, got {other:?}"),
    };
    let initial_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        initial_rendered.contains("data not available yet"),
        "expected the pre-refresh /status cell to render missing data before the request completes, got: {initial_rendered}"
    );

    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected refresh request for /status output, got {other:?}"),
    };

    chat.finish_status_rate_limit_refresh_as_unavailable(request_id);

    let refreshed_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        refreshed_rendered.contains("not available for this account"),
        "expected an empty successful refresh to become unavailable instead of staying missing, got: {refreshed_rendered}"
    );
    assert!(
        !refreshed_rendered.contains("data not available yet"),
        "expected the missing-data text to clear after the empty refresh completed, got: {refreshed_rendered}"
    );
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "expected the empty refresh to update the existing /status cell instead of inserting a second history cell"
    );
}

#[tokio::test]
async fn status_command_empty_successful_refresh_replaces_existing_cache_with_unavailable() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 55.0)));
    chat.dispatch_command(SlashCommand::Status);

    let cell = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected status output before refresh request, got {other:?}"),
    };
    let initial_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        initial_rendered.contains("55% left"),
        "expected the pre-refresh /status cell to use the existing cache, got: {initial_rendered}"
    );

    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected refresh request for cached /status output, got {other:?}"),
    };

    chat.replace_rate_limit_snapshots(Vec::new());
    chat.finish_status_rate_limit_refresh_as_unavailable(request_id);

    let refreshed_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        refreshed_rendered.contains("not available for this account"),
        "expected a successful empty refresh to replace stale cached limits with unavailable, got: {refreshed_rendered}"
    );
    assert!(
        !refreshed_rendered.contains("55% left"),
        "expected cached rows to clear after the empty refresh completed, got: {refreshed_rendered}"
    );
}

#[tokio::test]
async fn status_command_unavailable_refresh_persists_for_future_status_outputs() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);
    assert_matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_)));
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected refresh request for /status output, got {other:?}"),
    };

    chat.finish_status_rate_limit_refresh_as_unavailable(request_id);

    chat.dispatch_command(SlashCommand::Status);
    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected later status output, got {other:?}"),
    };
    assert!(
        rendered.contains("not available for this account"),
        "expected future /status output to preserve the last known unavailable state, got: {rendered}"
    );
    assert!(
        !rendered.contains("data not available yet"),
        "expected unavailable to persist instead of regressing to missing, got: {rendered}"
    );
}

#[tokio::test]
async fn active_account_change_clears_cached_limits_and_prefetches() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 55.0)));

    chat.on_active_account_changed();

    assert!(chat.rate_limit_snapshots_by_limit_id.is_empty());
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StartupPrefetch,
            account_generation: 1,
        })
    );

    chat.dispatch_command(SlashCommand::Status);
    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected /status output after account switch, got {other:?}"),
    };
    assert!(
        rendered.contains("data not available yet"),
        "expected account switch to clear the previous account cache before /status renders, got: {rendered}"
    );
    assert!(
        !rendered.contains("55% left"),
        "expected account switch to prevent stale cached limits from rendering, got: {rendered}"
    );
}

#[tokio::test]
async fn active_account_change_resets_warning_state_and_advances_generation() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.rate_limit_warnings.primary_index = 2;
    chat.rate_limit_warnings.secondary_index = 1;
    chat.rate_limit_account_generation = 41;

    chat.on_active_account_changed();

    pretty_assertions::assert_eq!(chat.rate_limit_warnings.primary_index, 0);
    pretty_assertions::assert_eq!(chat.rate_limit_warnings.secondary_index, 0);
    pretty_assertions::assert_eq!(chat.rate_limit_account_generation, 42);
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StartupPrefetch,
            account_generation: 42,
        })
    );
}

#[tokio::test]
async fn stale_status_refresh_does_not_repaint_card_from_new_account_cache() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 55.0)));

    chat.dispatch_command(SlashCommand::Status);
    let cell = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected status output before refresh request, got {other:?}"),
    };
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected refresh request for /status output, got {other:?}"),
    };

    let rendered_before = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        rendered_before.contains("55% left"),
        "expected original /status card to render the old account cache, got: {rendered_before}"
    );

    chat.on_active_account_changed();
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StartupPrefetch,
            account_generation: 1,
        })
    );
    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 88.0)));

    chat.finish_status_rate_limit_refresh_without_change(request_id);

    let rendered_after = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        rendered_after.contains("55% left"),
        "expected stale refresh completion to preserve the original card data, got: {rendered_after}"
    );
    assert!(
        !rendered_after.contains("88% left"),
        "expected stale refresh completion to avoid repainting from the next account cache, got: {rendered_after}"
    );
}

#[tokio::test]
async fn status_command_failed_refresh_without_cache_stays_missing() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    let cell = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected status output before refresh request, got {other:?}"),
    };
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            account_generation: 0,
        }) => request_id,
        other => panic!("expected refresh request for /status output, got {other:?}"),
    };

    chat.finish_status_rate_limit_refresh(request_id);

    let refreshed_rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(
        refreshed_rendered.contains("data not available yet"),
        "expected a failed refresh without cache to stay missing instead of becoming unavailable, got: {refreshed_rendered}"
    );
    assert!(
        !refreshed_rendered.contains("not available for this account"),
        "expected a failed refresh without cache not to mislabel the account as unavailable, got: {refreshed_rendered}"
    );
}
