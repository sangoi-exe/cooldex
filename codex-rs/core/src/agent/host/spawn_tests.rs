#![cfg(test)]

//! Tests live child bindings and the capture boundary for detached child authority.

use super::SpawnDestination;
use super::prepare_spawn;
use crate::HostThreadSpawnReservation;
use crate::HostToolInvocationLookup;
use crate::ThreadManager;
use crate::agent::host::InvocationReservation;
use crate::agent::types::SpawnAgentForkMode;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::thread_manager::StartThreadOptions;
use crate::thread_spawn::ThreadCreateRequest;
use crate::thread_spawn::ThreadSpawnError;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use codex_config::ScopedSkillsConfig;
use codex_exec_server::ExecServerError;
use codex_exec_server::RemoteEnvironmentOptions;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::ThreadId;
use codex_protocol::approvals::ExecPolicyAmendment;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use std::future::poll_fn;
use std::net::TcpListener;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn spawn_fixture() -> anyhow::Result<(tempfile::TempDir, Config, ThreadManager)> {
    let home = tempfile::tempdir()?;
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        home.path().to_path_buf(),
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
    );
    Ok((home, config, manager))
}

fn child_request(fork_mode: Option<SpawnAgentForkMode>) -> ThreadCreateRequest {
    ThreadCreateRequest {
        task_name: "worker".into(),
        fork_mode,
        agent_type: None,
        model: None,
        reasoning_effort: None,
    }
}

#[tokio::test]
async fn same_manager_creation_retains_step_executor_and_shares_exec_policy() -> anyhow::Result<()>
{
    let (_home, mut config, manager) = spawn_fixture().await?;
    config.features.enable(Feature::MultiAgentV2)?;
    config.features.enable(Feature::DeferredExecutor)?;
    let environments = manager.environment_manager();
    // Leave the handshake pending so the registration stays attached throughout the spawn.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let url = format!("ws://{}", listener.local_addr()?);
    environments.upsert_environment_with_options(
        "workspace".into(),
        RemoteEnvironmentOptions {
            exec_server_url: url.clone(),
            connect_timeout: Some(Duration::from_secs(300)),
            http_headers: Default::default(),
        },
        ScopedSkillsConfig {
            required: vec!["original-skill".into()],
        },
    )?;
    let mut requests = manager.default_environment_requests(&config.cwd, &config.workspace_roots);
    requests
        .iter_mut()
        .find(|request| request.environment_id == "workspace")
        .expect("registered workspace")
        .config = EnvironmentConfigState::Pending;
    let parent = manager
        .start_thread(StartThreadOptions {
            environments: Some(requests),
            ..StartThreadOptions::new(config)
        })
        .await?
        .thread;
    parent
        .thread_extension_data()
        .insert(HostToolInvocationLookup);
    let session = &parent.session;
    let step = session
        .capture_step_context(session.new_default_turn().await, &CancellationToken::new())
        .await?;
    let call = ToolInvocation {
        session: Arc::clone(session),
        turn: Arc::clone(&step.turn),
        step_context: step,
        cancellation_token: CancellationToken::new(),
        tracker: Default::default(),
        call_id: "spawn-call".into(),
        tool_name: ToolName::plain("host_spawn"),
        source: ToolCallSource::Direct,
        payload: ToolPayload::Function {
            arguments: "{}".into(),
        },
        environments: Default::default(),
    };
    let reservation = InvocationReservation::default();
    reservation.reserve(session, &call.turn, &call.call_id)?;
    assert!(
        parent
            .tool_invocation(&call.turn.sub_id, &call.call_id)
            .is_err()
    );
    let guard = reservation.activate(&call);
    let invocation = parent.tool_invocation(&call.turn.sub_id, &call.call_id)?;
    environments.upsert_environment("workspace".into(), url, /*connect_timeout*/ None)?;
    assert!(matches!(
        parent.prepare_child(&invocation, child_request(/*fork_mode*/ None)).await,
        Err(ThreadSpawnError::Backend(error)) if error.to_string().contains("resolved environment configuration")
    ));
    let child_id = ThreadId::new();
    parent
        .create_child(
            &invocation,
            child_request(/*fork_mode*/ None),
            HostThreadSpawnReservation {
                thread_id: child_id,
                control: Arc::clone(&session.services.agent_control),
            },
        )
        .await?;
    let child = manager.get_thread(child_id).await?;
    assert!(
        child
            .thread_extension_data()
            .get::<HostToolInvocationLookup>()
            .is_some()
    );
    let snapshot = child.session.services.turn_environments.snapshot_now();
    assert_eq!(
        snapshot
            .required_skills()
            .find(|required| required.environment_id == "workspace")
            .expect("child retained the original executor registration")
            .skill_names,
        ["original-skill"]
    );
    let pending = snapshot
        .starting()
        .find(|starting| starting.selection.environment_id == "workspace")
        .expect("child awaits the workspace");
    let failure = "owner failed to configure workspace";
    parent
        .environment_failed(&pending.selection, failure.into())
        .await?;
    let error = tokio::time::timeout(Duration::from_secs(5), pending.wait_until_ready())
        .await?
        .expect_err("child should receive the owner's failure");
    assert!(matches!(error.as_ref(), ExecServerError::Protocol(message) if message == failure));
    assert!(Arc::ptr_eq(
        &session.services.exec_policy,
        &child.session.services.exec_policy,
    ));
    drop(guard);
    assert!(
        parent
            .tool_invocation(&call.turn.sub_id, &call.call_id)
            .is_err()
    );
    drop(reservation);
    let next = InvocationReservation::default();
    next.reserve(session, &call.turn, &call.call_id)?;
    let _next_guard = next.activate(&call);
    parent.tool_invocation(&call.turn.sub_id, &call.call_id)?;
    assert!(matches!(
        parent.prepare_child(&invocation, child_request(/*fork_mode*/ None)).await,
        Err(ThreadSpawnError::Preparation(message)) if message == "spawning tool call finished"
    ));
    child.shutdown_and_wait().await?;
    parent.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn prepared_spawn_captures_policy_after_fork_history() -> anyhow::Result<()> {
    let (home, config, manager) = spawn_fixture().await?;
    let parent = manager
        .start_thread(StartThreadOptions::new(config))
        .await?
        .thread;
    let session = &parent.session;
    let step = session
        .capture_step_context(session.new_default_turn().await, &CancellationToken::new())
        .await?;
    let (request, path) = crate::thread_spawn::prepare(
        session,
        &step,
        "fork-call",
        child_request(Some(SpawnAgentForkMode::FullHistory)),
        None,
    )
    .await?;
    let rollout = parent
        .rollout_path()
        .expect("local parent has a rollout path");
    assert!(!rollout.try_exists()?);
    let mut preparation = std::pin::pin!(prepare_spawn(
        &parent,
        request,
        path,
        SpawnDestination::Detached,
    ));
    // Pause the fork as soon as it materializes the parent's rollout, then amend the policy.
    let materialized = poll_fn(|cx| {
        if rollout.exists() {
            return Poll::Ready(());
        }
        assert!(
            preparation.as_mut().poll(cx).is_pending(),
            "fork finished before materializing history"
        );
        Poll::Pending
    });
    tokio::time::timeout(Duration::from_secs(10), materialized).await?;
    let command = vec!["late_fork_policy_test".to_owned()];
    let parent_policy = &session.services.exec_policy;
    parent_policy
        .append_amendment_and_update(home.path(), &ExecPolicyAmendment::new(command.clone()))
        .await?;
    let prepared = tokio::time::timeout(Duration::from_secs(10), preparation).await??;
    let later = vec!["after_prepare_policy_test".to_owned()];
    parent_policy
        .append_amendment_and_update(home.path(), &ExecPolicyAmendment::new(later.clone()))
        .await?;
    let prefixes = prepared
        .inheritance
        .exec_policy
        .expect("detached policy")
        .current()
        .get_allowed_prefixes();
    assert!(prefixes.contains(&command));
    assert!(!prefixes.contains(&later));
    parent.shutdown_and_wait().await?;
    Ok(())
}

#[test_case::test_case(false; "creation_catalog")]
#[test_case::test_case(true; "explicit_clear_with_foreign_update")]
#[tokio::test]
async fn detached_full_history_preparation_captures_persisted_catalog(
    clear_parent_catalog: bool,
) -> anyhow::Result<()> {
    use codex_history::InitialHistory;
    use codex_history::RolloutItem;
    use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
    use codex_protocol::dynamic_tools::DynamicToolSpec;
    use codex_protocol::protocol::AgentUsageHintBinding;
    use codex_protocol::protocol::MultiAgentVersion;

    let (_home, mut config, manager) = spawn_fixture().await?;
    config.features.enable(Feature::MultiAgentV2)?;
    config.features.disable(Feature::MultiAgentV2DynamicTools)?;
    config.multi_agent_v2.root_agent_usage_hint_text = Some("captured host hint".into());
    let parent_tools = vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: "host_parent_tool".into(),
        description: "Parent catalog entry".into(),
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
        defer_loading: false,
    })];
    let parent = manager
        .start_thread(StartThreadOptions {
            environments: Some(Vec::new()),
            dynamic_tools: parent_tools.clone(),
            ..StartThreadOptions::new(config)
        })
        .await?
        .thread;
    let session = &parent.session;
    assert_eq!(
        session.set_multi_agent_version_if_unset(MultiAgentVersion::V2),
        MultiAgentVersion::V2
    );
    parent.ensure_rollout_materialized().await;
    if clear_parent_catalog {
        assert!(
            session
                .persist_rollout_items(&[
                    RolloutItem::DynamicToolsUpdated {
                        thread_id: session.thread_id,
                        tools: Vec::new()
                    },
                    RolloutItem::DynamicToolsUpdated {
                        thread_id: ThreadId::new(),
                        tools: parent_tools.clone()
                    },
                ])
                .await
        );
    }
    parent.flush_rollout().await?;
    assert_eq!(
        session.dynamic_tools().await.as_ref(),
        parent_tools.as_slice()
    );
    let step = session
        .capture_step_context(session.new_default_turn().await, &CancellationToken::new())
        .await?;
    let (request, path) = crate::thread_spawn::prepare(
        session,
        &step,
        "host-fork-call",
        child_request(Some(SpawnAgentForkMode::FullHistory)),
        None,
    )
    .await?;
    let prepared = prepare_spawn(&parent, request, path, SpawnDestination::Detached).await?;
    parent.shutdown_and_wait().await?;
    drop(manager);

    let expected_tools = if clear_parent_catalog {
        Vec::new()
    } else {
        parent_tools
    };
    assert_eq!(prepared.thread_options.dynamic_tools, expected_tools);
    assert!(matches!(
        &prepared.thread_options.config.agent_usage_hint_binding,
        AgentUsageHintBinding::Inherited { instructions: Some(instructions) }
            if instructions.text == "captured host hint"
    ));
    let InitialHistory::Forked(items) = &prepared.thread_options.initial_history else {
        panic!("expected captured fork history");
    };
    assert!(!items.iter().any(|item| matches!(
        item,
        RolloutItem::SessionMeta(_) | RolloutItem::DynamicToolsUpdated { .. }
    )));
    assert_eq!(codex_history::latest_dynamic_tools(items), None);
    Ok(())
}
