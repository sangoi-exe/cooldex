//! Config reload, persistence, and runtime config mutation helpers for the TUI app.
//!
//! This module keeps app-owned config writes and in-memory config refreshes together so
//! app-server-backed reloads and local TUI followers stay aligned.

use super::*;

pub(super) fn config_edits_builder_for_config(config: &Config) -> ConfigEditsBuilder {
    let builder = ConfigEditsBuilder::new(&config.codex_home);
    match config.active_user_config_path() {
        Ok(active_config_path) => builder.user_config_path(active_config_path),
        Err(err) => {
            tracing::debug!(
                error = %err,
                "active user config path is unavailable; using codex_home config path for TUI config persistence"
            );
            builder
        }
    }
}

impl App {
    pub(super) fn config_edits_builder(&self) -> ConfigEditsBuilder {
        config_edits_builder_for_config(&self.config)
    }

    pub(super) async fn rebuild_config_for_cwd(
        &self,
        cwd: PathBuf,
        config_path: Option<PathBuf>,
    ) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        let mut builder = ConfigBuilder::default()
            .codex_home(self.config.codex_home.to_path_buf())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides);
        if let Some(config_path) = config_path {
            builder = builder.user_config_path(Some(config_path));
        }
        builder
            .build()
            .await
            .wrap_err_with(|| format!("Failed to rebuild config for cwd {cwd_display}"))
    }

    pub(super) async fn refresh_in_memory_config_from_disk(&mut self) -> Result<()> {
        let displayed_thread_id = self.current_displayed_thread_id();
        let config_path = match displayed_thread_id {
            Some(thread_id) => self
                .thread_config_path(thread_id)
                .await
                .or_else(|| self.config.active_user_config_path().ok()),
            None => self.config.active_user_config_path().ok(),
        };
        let mut config = self
            .rebuild_config_for_cwd(self.chat_widget.config_ref().cwd.to_path_buf(), config_path)
            .await?;
        self.apply_runtime_policy_overrides(&mut config);
        self.config = config;
        self.auth_manager
            .set_forced_chatgpt_workspace_id(self.config.forced_chatgpt_workspace_id.clone());
        self.chat_widget.sync_plugin_mentions_config(&self.config);
        self.chat_widget.refresh_plugin_mentions();
        Ok(())
    }

    // Merge-safety anchor: WS1-B live config reload must keep app-server runtime reload,
    // local config rebuild, plugin mentions, and canonical `skills/list` follower refresh in
    // one ordered flow so TUI-visible skill/config state cannot split owners again.
    pub(super) async fn reload_live_user_config_and_followers(
        &mut self,
        app_server: &mut AppServerSession,
    ) -> Result<()> {
        app_server
            .reload_user_config()
            .await
            .wrap_err("failed to reload live config in app-server-backed TUI")?;
        self.refresh_in_memory_config_from_disk()
            .await
            .wrap_err("failed to rebuild local TUI config after live config reload")?;
        let response = app_server
            .skills_list(codex_app_server_protocol::SkillsListParams {
                cwds: Vec::new(),
                force_reload: true,
                per_cwd_extra_user_roots: None,
            })
            .await
            .wrap_err("skills/list failed while refreshing live config followers in TUI")?;
        self.handle_skills_list_response(response);
        Ok(())
    }

    pub(super) async fn set_skill_enabled_via_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        path: PathBuf,
        enabled: bool,
    ) -> Result<()> {
        let path_display = path.display().to_string();
        let absolute_path = AbsolutePathBuf::try_from(path)
            .wrap_err_with(|| format!("skill path `{path_display}` must be absolute"))?;
        app_server
            .skills_config_write(absolute_path, enabled)
            .await
            .wrap_err_with(|| format!("skills/config/write failed for {path_display}"))?;
        self.reload_live_user_config_and_followers(app_server)
            .await
            .wrap_err_with(|| {
                format!("updated skill config for {path_display}, but failed to reload live config")
            })?;
        Ok(())
    }

    pub(super) async fn set_app_enabled_via_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        id: String,
        enabled: bool,
    ) -> Result<()> {
        let enabled_key_path = join_config_key_path_segments(["apps", id.as_str(), "enabled"]);
        let disabled_reason_key_path =
            join_config_key_path_segments(["apps", id.as_str(), "disabled_reason"]);
        let edits = if enabled {
            vec![
                AppServerConfigEdit {
                    key_path: enabled_key_path.clone(),
                    value: serde_json::Value::Null,
                    merge_strategy: AppServerMergeStrategy::Replace,
                },
                AppServerConfigEdit {
                    key_path: disabled_reason_key_path.clone(),
                    value: serde_json::Value::Null,
                    merge_strategy: AppServerMergeStrategy::Replace,
                },
            ]
        } else {
            vec![
                AppServerConfigEdit {
                    key_path: enabled_key_path,
                    value: false.into(),
                    merge_strategy: AppServerMergeStrategy::Replace,
                },
                AppServerConfigEdit {
                    key_path: disabled_reason_key_path,
                    value: "user".into(),
                    merge_strategy: AppServerMergeStrategy::Replace,
                },
            ]
        };
        let write_result = app_server
            .config_batch_write_and_reload_user_config(edits)
            .await
            .wrap_err_with(|| format!("failed to update app config for {id}"));
        let refresh_result = if write_result.is_ok() {
            self.refresh_in_memory_config_from_disk()
                .await
                .wrap_err_with(|| {
                    format!("updated app config for {id}, but failed to rebuild local TUI config")
                })
        } else {
            Ok(())
        };
        self.finish_set_app_enabled_after_canonical_write(
            &id,
            enabled,
            write_result,
            refresh_result,
        )
    }

    pub(super) fn finish_set_app_enabled_after_canonical_write(
        &mut self,
        id: &str,
        enabled: bool,
        write_result: Result<()>,
        refresh_result: Result<()>,
    ) -> Result<()> {
        write_result?;
        refresh_result?;
        self.chat_widget.update_connector_enabled(id, enabled);
        Ok(())
    }

    pub(super) async fn refresh_in_memory_config_from_disk_best_effort(&mut self, action: &str) {
        if let Err(err) = self.refresh_in_memory_config_from_disk().await {
            tracing::warn!(
                error = %err,
                action,
                "failed to refresh config before thread transition; continuing with current in-memory config"
            );
        }
    }

    pub(super) async fn rebuild_config_for_resume_or_fallback(
        &mut self,
        current_cwd: &Path,
        resume_cwd: PathBuf,
        config_path: Option<PathBuf>,
    ) -> Result<Config> {
        let explicit_config_path = config_path.is_some();
        match self
            .rebuild_config_for_cwd(resume_cwd.clone(), config_path)
            .await
        {
            Ok(config) => Ok(config),
            Err(err) => {
                if crate::cwds_differ(current_cwd, &resume_cwd) || explicit_config_path {
                    Err(err)
                } else {
                    let resume_cwd_display = resume_cwd.display().to_string();
                    tracing::warn!(
                        error = %err,
                        cwd = %resume_cwd_display,
                        "failed to rebuild config for same-cwd resume; using current in-memory config"
                    );
                    Ok(self.config.clone())
                }
            }
        }
    }

    pub(super) fn apply_runtime_policy_overrides(&mut self, config: &mut Config) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.permissions.approval_policy.set(*policy)
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(policy) = self.runtime_sandbox_policy_override.as_ref()
            && let Err(err) = config.permissions.sandbox_policy.set(policy.clone())
        {
            tracing::warn!(%err, "failed to carry forward sandbox policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward sandbox policy override: {err}"
            ));
        }
    }

    pub(super) fn set_approvals_reviewer_in_app_and_widget(&mut self, reviewer: ApprovalsReviewer) {
        self.config.approvals_reviewer = reviewer;
        self.chat_widget.set_approvals_reviewer(reviewer);
    }

    pub(super) fn try_set_approval_policy_on_config(
        &mut self,
        config: &mut Config,
        policy: AskForApproval,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.approval_policy.set(policy) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    pub(super) fn try_set_sandbox_policy_on_config(
        &mut self,
        config: &mut Config,
        policy: SandboxPolicy,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.sandbox_policy.set(policy) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    pub(super) async fn update_feature_flags(&mut self, updates: Vec<(Feature, bool)>) {
        if updates.is_empty() {
            return;
        }

        let guardian_approvals_preset = guardian_approval_preset();
        let mut next_config = self.config.clone();
        let active_profile = self.active_profile.clone();
        let scoped_segments = |key: &str| {
            if let Some(profile) = active_profile.as_deref() {
                vec!["profiles".to_string(), profile.to_string(), key.to_string()]
            } else {
                vec![key.to_string()]
            }
        };
        let windows_sandbox_changed = updates.iter().any(|(feature, _)| {
            matches!(
                feature,
                Feature::WindowsSandbox | Feature::WindowsSandboxElevated
            )
        });
        let mut approval_policy_override = None;
        let mut approvals_reviewer_override = None;
        let mut sandbox_policy_override = None;
        let mut feature_updates_to_apply = Vec::with_capacity(updates.len());
        let mut permissions_history_label: Option<&'static str> = None;
        let mut builder = self
            .config_edits_builder()
            .with_profile(self.active_profile.as_deref());

        for (feature, enabled) in updates {
            let feature_key = feature.key();
            let mut feature_edits = Vec::new();
            let mut feature_config = next_config.clone();
            if let Err(err) = feature_config.features.set_enabled(feature, enabled) {
                tracing::error!(
                    error = %err,
                    feature = feature_key,
                    "failed to update constrained feature flags"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to update experimental feature `{feature_key}`: {err}"
                ));
                continue;
            }
            let effective_enabled = feature_config.features.enabled(feature);
            if feature == Feature::GuardianApproval {
                // Merge-safety anchor: the feature flag is only Guardian preset discoverability/convenience;
                // reviewer + approval + sandbox remain the live routing owner.
                let previous_approvals_reviewer = feature_config.approvals_reviewer;
                if effective_enabled {
                    // Guardian Approvals routing is owned by the effective
                    // reviewer + approval policy, not by the experimental
                    // feature flag. Enabling the feature opts into the shared
                    // Guardian preset as a convenience.
                    feature_config.approvals_reviewer =
                        guardian_approvals_preset.approvals_reviewer;
                    feature_edits.push(ConfigEdit::SetPath {
                        segments: scoped_segments("approvals_reviewer"),
                        value: guardian_approvals_preset
                            .approvals_reviewer
                            .to_string()
                            .into(),
                    });
                    if previous_approvals_reviewer != guardian_approvals_preset.approvals_reviewer {
                        permissions_history_label = Some("Auto-review");
                    }
                    approvals_reviewer_override = Some(feature_config.approvals_reviewer);
                }
            }
            if feature == Feature::GuardianApproval && effective_enabled {
                // The feature flag alone is not enough for the live session.
                // We also align approval policy + sandbox to the Auto-review
                // preset so enabling the experiment immediately
                // makes guardian review observable in the current thread.
                if !self.try_set_approval_policy_on_config(
                    &mut feature_config,
                    guardian_approvals_preset.approval,
                    "Failed to enable Auto-review",
                    "failed to set guardian approvals approval policy on staged config",
                ) {
                    continue;
                }
                if !self.try_set_sandbox_policy_on_config(
                    &mut feature_config,
                    guardian_approvals_preset.sandbox.clone(),
                    "Failed to enable Auto-review",
                    "failed to set guardian approvals sandbox policy on staged config",
                ) {
                    continue;
                }
                feature_edits.extend([
                    ConfigEdit::SetPath {
                        segments: scoped_segments("approval_policy"),
                        value: "on-request".into(),
                    },
                    ConfigEdit::SetPath {
                        segments: scoped_segments("sandbox_mode"),
                        value: "workspace-write".into(),
                    },
                ]);
                approval_policy_override = Some(guardian_approvals_preset.approval);
                sandbox_policy_override = Some(guardian_approvals_preset.sandbox.clone());
            }
            next_config = feature_config;
            feature_updates_to_apply.push((feature, effective_enabled));
            builder = builder
                .with_edits(feature_edits)
                .set_feature_enabled(feature_key, effective_enabled);
        }

        // Persist first so the live session does not diverge from disk if the
        // config edit fails. Runtime/UI state is patched below only after the
        // durable config update succeeds.
        if let Err(err) = builder.apply().await {
            tracing::error!(error = %err, "failed to persist feature flags");
            self.chat_widget
                .add_error_message(format!("Failed to update experimental features: {err}"));
            return;
        }

        self.config = next_config;
        let show_memory_enable_notice = feature_updates_to_apply
            .iter()
            .any(|(feature, enabled)| *feature == Feature::MemoryTool && *enabled);
        for (feature, effective_enabled) in feature_updates_to_apply {
            self.chat_widget
                .set_feature_enabled(feature, effective_enabled);
        }
        if show_memory_enable_notice {
            self.chat_widget.add_memories_enable_notice();
        }
        if approvals_reviewer_override.is_some() {
            self.set_approvals_reviewer_in_app_and_widget(self.config.approvals_reviewer);
        }
        if approval_policy_override.is_some() {
            self.chat_widget
                .set_approval_policy(self.config.permissions.approval_policy.value());
        }
        if sandbox_policy_override.is_some()
            && let Err(err) = self
                .chat_widget
                .set_sandbox_policy(self.config.permissions.sandbox_policy.get().clone())
        {
            tracing::error!(
                error = %err,
                "failed to set guardian approvals sandbox policy on chat config"
            );
            self.chat_widget
                .add_error_message(format!("Failed to enable Auto-review: {err}"));
        }

        if approval_policy_override.is_some()
            || approvals_reviewer_override.is_some()
            || sandbox_policy_override.is_some()
        {
            // This uses `OverrideTurnContext` intentionally: toggling the
            // experiment should update the active thread's effective approval
            // settings immediately, just like a `/permissions` selection. Without
            // this runtime patch, the config edit would only affect future
            // sessions or turns recreated from disk.
            let op = AppCommand::override_turn_context(
                /*cwd*/ None,
                approval_policy_override,
                approvals_reviewer_override,
                sandbox_policy_override,
                /*windows_sandbox_level*/ None,
                /*model*/ None,
                /*effort*/ None,
                /*summary*/ None,
                /*service_tier*/ None,
                /*collaboration_mode*/ None,
                /*personality*/ None,
            );
            let replay_state_op =
                ThreadEventStore::op_can_change_pending_replay_state(&op).then(|| op.clone());
            let submitted = self.chat_widget.submit_op(op);
            if submitted && let Some(op) = replay_state_op.as_ref() {
                self.note_active_thread_outbound_op(op).await;
                self.refresh_pending_thread_approvals().await;
            }
        }

        if windows_sandbox_changed {
            #[cfg(target_os = "windows")]
            {
                let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
                self.app_event_tx.send(AppEvent::CodexOp(
                    AppCommand::override_turn_context(
                        /*cwd*/ None,
                        /*approval_policy*/ None,
                        /*approvals_reviewer*/ None,
                        /*sandbox_policy*/ None,
                        #[cfg(target_os = "windows")]
                        Some(windows_sandbox_level),
                        /*model*/ None,
                        /*effort*/ None,
                        /*summary*/ None,
                        /*service_tier*/ None,
                        /*collaboration_mode*/ None,
                        /*personality*/ None,
                    )
                    .into_core(),
                ));
            }
        }

        if let Some(label) = permissions_history_label {
            self.chat_widget.add_info_message(
                format!("Permissions updated to {label}"),
                /*hint*/ None,
            );
        }
    }

    pub(super) async fn update_memory_settings(
        &mut self,
        use_memories: bool,
        generate_memories: bool,
    ) -> bool {
        let active_profile = self.active_profile.clone();
        let scoped_memory_segments = |key: &str| {
            if let Some(profile) = active_profile.as_deref() {
                vec![
                    "profiles".to_string(),
                    profile.to_string(),
                    "memories".to_string(),
                    key.to_string(),
                ]
            } else {
                vec!["memories".to_string(), key.to_string()]
            }
        };
        let edits = [
            ConfigEdit::SetPath {
                segments: scoped_memory_segments("use_memories"),
                value: use_memories.into(),
            },
            ConfigEdit::SetPath {
                segments: scoped_memory_segments("generate_memories"),
                value: generate_memories.into(),
            },
        ];

        let builder = self.config_edits_builder();

        if let Err(err) = builder.with_edits(edits).apply().await {
            tracing::error!(error = %err, "failed to persist memory settings");
            self.chat_widget
                .add_error_message(format!("Failed to save memory settings: {err}"));
            return false;
        }

        self.config.memories.use_memories = use_memories;
        self.config.memories.generate_memories = generate_memories;
        self.chat_widget
            .set_memory_settings(use_memories, generate_memories);
        true
    }

    pub(super) async fn update_memory_settings_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        use_memories: bool,
        generate_memories: bool,
    ) {
        let previous_generate_memories = self.config.memories.generate_memories;
        if !self
            .update_memory_settings(use_memories, generate_memories)
            .await
        {
            return;
        }

        if previous_generate_memories == generate_memories {
            return;
        }

        let Some(thread_id) = self.current_displayed_thread_id() else {
            return;
        };

        let mode = if generate_memories {
            ThreadMemoryMode::Enabled
        } else {
            ThreadMemoryMode::Disabled
        };

        if let Err(err) = app_server.thread_memory_mode_set(thread_id, mode).await {
            tracing::error!(error = %err, %thread_id, "failed to update thread memory mode");
            self.chat_widget.add_error_message(format!(
                "Saved memory settings, but failed to update the current thread: {err}"
            ));
        }
    }

    pub(super) async fn reset_memories_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        if let Err(err) = app_server.memory_reset().await {
            tracing::error!(error = %err, "failed to reset memories");
            self.chat_widget
                .add_error_message(format!("Failed to reset memories: {err}"));
            return;
        }

        self.chat_widget
            .add_info_message("Reset local memories.".to_string(), /*hint*/ None);
    }

    pub(super) fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    pub(super) fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> codex_protocol::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    pub(super) fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort;
        self.chat_widget.set_reasoning_effort(effort);
    }

    pub(super) fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_widget.set_personality(personality);
    }

    pub(super) fn sync_tui_theme_selection(&mut self, name: String) {
        self.config.tui_theme = Some(name.clone());
        self.chat_widget.set_tui_theme(Some(name));
    }

    pub(super) fn restore_runtime_theme_from_config(&self) {
        if let Some(name) = self.config.tui_theme.as_deref()
            && let Some(theme) =
                crate::render::highlight::resolve_theme_by_name(name, Some(&self.config.codex_home))
        {
            crate::render::highlight::set_syntax_theme(theme);
            return;
        }

        let auto_theme_name = crate::render::highlight::adaptive_default_theme_name();
        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
            auto_theme_name,
            Some(&self.config.codex_home),
        ) {
            crate::render::highlight::set_syntax_theme(theme);
        }
    }

    pub(super) fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }
}
