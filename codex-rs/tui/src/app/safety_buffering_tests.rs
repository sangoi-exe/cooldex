use super::*;
use codex_app_server_protocol::TurnItemsView;

#[tokio::test]
async fn safety_buffering_choice_saves_selected_file_and_live_settings() -> anyhow::Result<()> {
    use codex_config::types::SafetyBufferingChoice;
    use codex_utils_absolute_path::AbsolutePathBuf;

    let home = tempfile::tempdir()?;
    let base = home.path().join("config.toml");
    let selected = home.path().join("work.config.toml");
    std::fs::write(&base, "# untouched base config\n")?;
    std::fs::write(
        &selected,
        "# selected profile\n[tui]\nanimations = false\nsafety_buffering_auto_apply = true\n",
    )?;
    let (mut app, _events, _ops) = crate::app::tests::make_test_app_with_channels().await;
    app.local_settings.user_config_path = AbsolutePathBuf::from_absolute_path(&selected)?;
    for choice in [
        SafetyBufferingChoice::DismissAndKeepWaiting,
        SafetyBufferingChoice::RetryWithFasterModel,
    ] {
        app.save_safety_buffering_choice(choice).await;
        assert_eq!(
            app.local_settings.tui.safety_buffering_last_choice,
            Some(choice)
        );
        assert_eq!(
            app.chat_widget
                .local_settings
                .tui
                .safety_buffering_last_choice,
            Some(choice)
        );
        let content = std::fs::read_to_string(&selected)?;
        assert!(content.contains("# selected profile"));
        let saved: codex_config::types::Tui = toml::from_str::<toml::Value>(&content)?["tui"]
            .clone()
            .try_into()?;
        assert_eq!(saved.safety_buffering_last_choice, Some(choice));
        assert!(saved.safety_buffering_auto_apply);
        assert!(!saved.animations);
        let reloaded = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(codex_config::LoaderOverrides {
                user_config_path: Some(app.local_settings.user_config_path.clone()),
                ignore_project_config: true,
                ..codex_config::LoaderOverrides::without_managed_config_for_tests()
            })
            .build()
            .await?;
        let settings = app.local_settings.reloaded(&reloaded);
        assert_eq!(settings.tui.safety_buffering_last_choice, Some(choice));
        assert!(settings.tui.safety_buffering_auto_apply);
    }
    assert_eq!(std::fs::read_to_string(base)?, "# untouched base config\n");
    Ok(())
}

#[tokio::test]
async fn safety_buffering_failed_save_preserves_live_choice() -> anyhow::Result<()> {
    use codex_config::types::SafetyBufferingChoice;
    use codex_utils_absolute_path::AbsolutePathBuf;

    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    std::fs::write(&path, "[invalid\n")?;
    let (mut app, mut events, _ops) = crate::app::tests::make_test_app_with_channels().await;
    app.local_settings.user_config_path = AbsolutePathBuf::from_absolute_path(&path)?;
    let before = (
        app.local_settings.clone(),
        app.chat_widget.local_settings.clone(),
    );
    app.save_safety_buffering_choice(SafetyBufferingChoice::DismissAndKeepWaiting)
        .await;
    assert_eq!(
        (&app.local_settings, &app.chat_widget.local_settings),
        (&before.0, &before.1)
    );
    assert_eq!(std::fs::read_to_string(path)?, "[invalid\n");
    let mut messages = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            messages.extend(cell.display_lines(80).iter().map(ToString::to_string));
        }
    }
    assert!(
        messages
            .join("\n")
            .contains("Failed to save safety-buffering choice")
    );
    Ok(())
}

fn turn(id: &str, status: TurnStatus) -> Turn {
    Turn {
        id: id.to_string(),
        items: Vec::new(),
        items_view: TurnItemsView::Full,
        status,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

#[test]
fn retry_rejects_a_stale_or_in_progress_turn() {
    let stale = vec![
        turn("turn-1", TurnStatus::Interrupted),
        turn("turn-2", TurnStatus::InProgress),
    ];
    let in_progress = vec![turn("turn-1", TurnStatus::InProgress)];
    let previous_in_progress = vec![
        turn("turn-1", TurnStatus::InProgress),
        turn("turn-2", TurnStatus::Interrupted),
    ];

    assert!(safety_retry_fork_point(&stale, "turn-1").is_err());
    assert!(safety_retry_fork_point(&in_progress, "turn-1").is_err());
    assert!(safety_retry_fork_point(&in_progress, "missing").is_err());
    assert!(safety_retry_fork_point(&previous_in_progress, "turn-2").is_err());
}

#[test]
fn retry_accepts_a_targeted_latest_turn_and_its_completed_predecessor() {
    let turns = vec![
        turn("turn-1", TurnStatus::Completed),
        turn("turn-2", TurnStatus::Interrupted),
    ];

    assert!(safety_retry_fork_point(&turns, "turn-2").is_ok());
    assert!(safety_retry_fork_point(&turns[1..], "turn-2").is_ok());
}
