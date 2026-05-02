//! Startup warnings, model migration prompts, and bootstrap prompt helpers.
//!
//! These helpers run before or during `App::run` bootstrap. They translate configuration and model
//! catalog state into one-time TUI prompts or warning cells without owning the main event loop.

use super::*;

pub(super) fn emit_skill_load_warnings(app_event_tx: &AppEventSender, errors: &[SkillErrorInfo]) {
    if errors.is_empty() {
        return;
    }

    let error_count = errors.len();
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_warning_event(format!(
            "Skipped loading {error_count} skill(s) due to invalid SKILL.md files."
        )),
    )));

    for error in errors {
        let path = error.path.display();
        let message = error.message.as_str();
        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            crate::history_cell::new_warning_event(format!("{path}: {message}")),
        )));
    }
}

pub(super) fn emit_project_config_warnings(app_event_tx: &AppEventSender, config: &Config) {
    let mut disabled_folders = Vec::new();

    for layer in config.config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ true,
    ) {
        let ConfigLayerSource::Project { dot_codex_folder } = &layer.name else {
            continue;
        };
        let Some(disabled_reason) = &layer.disabled_reason else {
            continue;
        };
        disabled_folders.push((
            dot_codex_folder.as_path().display().to_string(),
            disabled_reason.clone(),
        ));
    }

    if disabled_folders.is_empty() {
        return;
    }

    let mut message = concat!(
        "Project-local config, hooks, and exec policies are disabled in the following folders ",
        "until the project is trusted, but skills still load.\n",
    )
    .to_string();
    for (index, (folder, reason)) in disabled_folders.iter().enumerate() {
        let display_index = index + 1;
        message.push_str(&format!("    {display_index}. {folder}\n"));
        message.push_str(&format!("       {reason}\n"));
    }

    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        history_cell::new_warning_event(message),
    )));
}

pub(super) fn emit_system_bwrap_warning(app_event_tx: &AppEventSender, config: &Config) {
    let Some(message) = crate::legacy_core::config::system_bwrap_warning(
        config.permissions.permission_profile.get(),
    ) else {
        return;
    };

    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        history_cell::new_warning_event(message),
    )));
}

pub(super) fn should_show_model_migration_prompt(
    current_model: &str,
    target_model: &str,
    seen_migrations: &BTreeMap<String, String>,
    available_models: &[ModelPreset],
) -> bool {
    if target_model == current_model {
        return false;
    }

    if let Some(seen_target) = seen_migrations.get(current_model)
        && seen_target == target_model
    {
        return false;
    }

    if !available_models
        .iter()
        .any(|preset| preset.model == target_model && preset.show_in_picker)
    {
        return false;
    }

    if available_models
        .iter()
        .any(|preset| preset.model == current_model && preset.upgrade.is_some())
    {
        return true;
    }

    if available_models
        .iter()
        .any(|preset| preset.upgrade.as_ref().map(|u| u.id.as_str()) == Some(target_model))
    {
        return true;
    }

    false
}

pub(super) fn target_preset_for_upgrade<'a>(
    available_models: &'a [ModelPreset],
    target_model: &str,
) -> Option<&'a ModelPreset> {
    available_models
        .iter()
        .find(|preset| preset.model == target_model && preset.show_in_picker)
}

pub(super) const MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StartupTooltipOverride {
    pub(super) model_slug: String,
    pub(super) message: String,
}

pub(super) fn select_model_availability_nux(
    available_models: &[ModelPreset],
    nux_config: &ModelAvailabilityNuxConfig,
) -> Option<StartupTooltipOverride> {
    available_models.iter().find_map(|preset| {
        let ModelAvailabilityNux { message } = preset.availability_nux.as_ref()?;
        let shown_count = nux_config
            .shown_count
            .get(&preset.model)
            .copied()
            .unwrap_or_default();
        (shown_count < MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT).then(|| StartupTooltipOverride {
            model_slug: preset.model.clone(),
            message: message.clone(),
        })
    })
}

pub(super) async fn prepare_startup_tooltip_override(
    config: &mut Config,
    available_models: &[ModelPreset],
    is_first_run: bool,
) -> Option<String> {
    if is_first_run || !config.show_tooltips {
        return None;
    }

    let tooltip_override =
        select_model_availability_nux(available_models, &config.model_availability_nux)?;

    let shown_count = config
        .model_availability_nux
        .shown_count
        .get(&tooltip_override.model_slug)
        .copied()
        .unwrap_or_default();
    let next_count = shown_count.saturating_add(1);
    let mut updated_shown_count = config.model_availability_nux.shown_count.clone();
    updated_shown_count.insert(tooltip_override.model_slug.clone(), next_count);

    let builder = super::config_persistence::config_edits_builder_for_config(config);

    if let Err(err) = builder
        .set_model_availability_nux_count(&updated_shown_count)
        .apply()
        .await
    {
        tracing::error!(
            error = %err,
            model = %tooltip_override.model_slug,
            "failed to persist model availability nux count"
        );
        return Some(tooltip_override.message);
    }

    config.model_availability_nux.shown_count = updated_shown_count;
    Some(tooltip_override.message)
}

pub(super) async fn handle_model_migration_prompt_if_needed(
    tui: &mut tui::Tui,
    config: &mut Config,
    model: &str,
    app_event_tx: &AppEventSender,
    available_models: &[ModelPreset],
) -> Option<AppExitInfo> {
    let upgrade = available_models
        .iter()
        .find(|preset| preset.model == model)
        .and_then(|preset| preset.upgrade.as_ref());

    if let Some(ModelUpgrade {
        id: target_model,
        reasoning_effort_mapping,
        model_link,
        upgrade_copy,
        migration_markdown,
    }) = upgrade
    {
        let target_model = target_model.to_string();
        if !should_show_model_migration_prompt(
            model,
            &target_model,
            &config.notices.model_migrations,
            available_models,
        ) {
            return None;
        }

        let current_preset = available_models.iter().find(|preset| preset.model == model);
        let target_preset = target_preset_for_upgrade(available_models, &target_model);
        let target_preset = target_preset?;
        let target_display_name = target_preset.display_name.clone();
        let heading_label = if target_display_name == model {
            target_model.clone()
        } else {
            target_display_name.clone()
        };
        let target_description =
            (!target_preset.description.is_empty()).then(|| target_preset.description.clone());
        let can_opt_out = current_preset.is_some();
        let prompt_copy = migration_copy_for_models(
            model,
            &target_model,
            model_link.clone(),
            upgrade_copy.clone(),
            migration_markdown.clone(),
            heading_label,
            target_description,
            can_opt_out,
        );
        match run_model_migration_prompt(tui, prompt_copy).await {
            ModelMigrationOutcome::Accepted => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });

                let mapped_effort = if let Some(reasoning_effort_mapping) = reasoning_effort_mapping
                    && let Some(reasoning_effort) = config.model_reasoning_effort
                {
                    reasoning_effort_mapping
                        .get(&reasoning_effort)
                        .cloned()
                        .or(config.model_reasoning_effort)
                } else {
                    config.model_reasoning_effort
                };

                config.model = Some(target_model.clone());
                config.model_reasoning_effort = mapped_effort;
                app_event_tx.send(AppEvent::UpdateModel(target_model.clone()));
                app_event_tx.send(AppEvent::UpdateReasoningEffort(mapped_effort));
                app_event_tx.send(AppEvent::PersistModelSelection {
                    model: target_model.clone(),
                    effort: mapped_effort,
                });
            }
            ModelMigrationOutcome::Rejected => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });
            }
            ModelMigrationOutcome::Exit => {
                return Some(AppExitInfo {
                    token_usage: TokenUsage::default(),
                    thread_id: None,
                    thread_name: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
        }
    }

    None
}
pub(super) fn normalize_harness_overrides_for_cwd(
    mut overrides: ConfigOverrides,
    base_cwd: &AbsolutePathBuf,
) -> Result<ConfigOverrides> {
    if overrides.additional_writable_roots.is_empty() {
        return Ok(overrides);
    }

    let mut normalized = Vec::with_capacity(overrides.additional_writable_roots.len());
    for root in overrides.additional_writable_roots.drain(..) {
        let absolute = base_cwd.join(root);
        normalized.push(absolute.into_path_buf());
    }
    overrides.additional_writable_roots = normalized;
    Ok(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::test_support::PathBufExt;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
        let temp_dir = tempdir()?;
        let base_cwd = temp_dir.path().join("base").abs();
        std::fs::create_dir_all(base_cwd.as_path())?;

        let overrides = ConfigOverrides {
            additional_writable_roots: vec![PathBuf::from("rel")],
            ..Default::default()
        };
        let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

        assert_eq!(
            normalized.additional_writable_roots,
            vec![base_cwd.join("rel").into_path_buf()]
        );
        Ok(())
    }
    #[tokio::test]
    async fn model_migration_prompt_only_shows_for_deprecated_models() {
        let seen = BTreeMap::new();
        assert!(should_show_model_migration_prompt(
            "gpt-5.2",
            "gpt-5.4",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5.3-codex",
            "gpt-5.4",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.3-codex",
            "gpt-5.3-codex",
            &seen,
            &all_model_presets()
        ));
    }
    #[test]
    fn select_model_availability_nux_picks_only_eligible_model() {
        let mut presets = all_model_presets();
        presets.iter_mut().for_each(|preset| {
            preset.availability_nux = None;
        });
        let target = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4")
            .expect("target preset present");
        target.availability_nux = Some(ModelAvailabilityNux {
            message: "gpt-5.4 is available".to_string(),
        });

        let selected = select_model_availability_nux(&presets, &model_availability_nux_config(&[]));

        assert_eq!(
            selected,
            Some(StartupTooltipOverride {
                model_slug: "gpt-5.4".to_string(),
                message: "gpt-5.4 is available".to_string(),
            })
        );
    }
    #[test]
    fn select_model_availability_nux_skips_missing_and_exhausted_models() {
        let mut presets = all_model_presets();
        presets.iter_mut().for_each(|preset| {
            preset.availability_nux = None;
        });
        let gpt_5 = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4")
            .expect("gpt-5.4 preset present");
        gpt_5.availability_nux = Some(ModelAvailabilityNux {
            message: "gpt-5.4 is available".to_string(),
        });
        let gpt_5_2 = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4-mini")
            .expect("gpt-5.4-mini preset present");
        gpt_5_2.availability_nux = Some(ModelAvailabilityNux {
            message: "gpt-5.4-mini is available".to_string(),
        });

        let selected = select_model_availability_nux(
            &presets,
            &model_availability_nux_config(&[("gpt-5.4", MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT)]),
        );

        assert_eq!(
            selected,
            Some(StartupTooltipOverride {
                model_slug: "gpt-5.4-mini".to_string(),
                message: "gpt-5.4-mini is available".to_string(),
            })
        );
    }
    #[test]
    fn select_model_availability_nux_uses_existing_model_order_as_priority() {
        let mut presets = all_model_presets();
        presets.iter_mut().for_each(|preset| {
            preset.availability_nux = None;
        });
        let first = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4-mini")
            .expect("gpt-5.4-mini preset present");
        first.availability_nux = Some(ModelAvailabilityNux {
            message: "first".to_string(),
        });
        let second = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4")
            .expect("gpt-5.4 preset present");
        second.availability_nux = Some(ModelAvailabilityNux {
            message: "second".to_string(),
        });

        let selected = select_model_availability_nux(&presets, &model_availability_nux_config(&[]));

        assert_eq!(
            selected,
            Some(StartupTooltipOverride {
                model_slug: "gpt-5.4".to_string(),
                message: "second".to_string(),
            })
        );
    }
    #[test]
    fn select_model_availability_nux_returns_none_when_all_models_are_exhausted() {
        let mut presets = all_model_presets();
        presets.iter_mut().for_each(|preset| {
            preset.availability_nux = None;
        });
        let target = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4")
            .expect("target preset present");
        target.availability_nux = Some(ModelAvailabilityNux {
            message: "gpt-5.4 is available".to_string(),
        });

        let selected = select_model_availability_nux(
            &presets,
            &model_availability_nux_config(&[("gpt-5.4", MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT)]),
        );

        assert_eq!(selected, None);
    }
    #[tokio::test]
    async fn model_migration_prompt_respects_seen_mapping_and_self_target() {
        let mut seen = BTreeMap::new();
        seen.insert("gpt-5.2".to_string(), "gpt-5.4".to_string());
        assert!(!should_show_model_migration_prompt(
            "gpt-5.2",
            "gpt-5.4",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.4",
            "gpt-5.4",
            &seen,
            &all_model_presets()
        ));
    }
    #[tokio::test]
    async fn model_migration_prompt_skips_when_target_missing_or_hidden() {
        let mut available = all_model_presets();
        let mut current = available
            .iter()
            .find(|preset| preset.model == "gpt-5.2")
            .cloned()
            .expect("preset present");
        current.upgrade = Some(ModelUpgrade {
            id: "missing-target".to_string(),
            reasoning_effort_mapping: None,
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
        });
        available.retain(|preset| preset.model != "gpt-5.2");
        available.push(current.clone());

        assert!(!should_show_model_migration_prompt(
            &current.model,
            "missing-target",
            &BTreeMap::new(),
            &available,
        ));

        assert!(target_preset_for_upgrade(&available, "missing-target").is_none());

        let mut with_hidden_target = all_model_presets();
        let target = with_hidden_target
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.4")
            .expect("target preset present");
        target.show_in_picker = false;

        assert!(!should_show_model_migration_prompt(
            "gpt-5.2",
            "gpt-5.4",
            &BTreeMap::new(),
            &with_hidden_target,
        ));
        assert!(target_preset_for_upgrade(&with_hidden_target, "gpt-5.4").is_none());
    }
}
