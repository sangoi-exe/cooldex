use anyhow::Result;
use codex_core::config::Config;
use codex_features::Feature;
use codex_features::MultiAgentV2Policy;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const NO_SPAWN_TEXT: &str = "Any earlier instruction enabling proactive multi-agent delegation no longer applies. Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents, delegation, or parallel agent work.";
const PROACTIVE_TEXT: &str = "Proactive multi-agent delegation is active.";
const MODE_EXPLANATION_TEXT: &str = "Use the configured delegation policy.";
const ROOT_USAGE_HINT_TEXT: &str = "Root usage hint.";

fn add_ultra_reasoning(model_info: &mut ModelInfo) {
    model_info
        .supported_reasoning_levels
        .push(ReasoningEffortPreset {
            effort: ReasoningEffort::Ultra,
            description: "Ultra".to_string(),
        });
}

fn configure_multi_agent_v2(config: &mut Config) {
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
}

fn configure_mode_explanation(config: &mut Config) {
    configure_multi_agent_v2(config);
    config.multi_agent_v2.multi_agent_mode_hint_text = Some(MODE_EXPLANATION_TEXT.to_string());
}

fn configure_proactive(config: &mut Config) {
    configure_multi_agent_v2(config);
    config.multi_agent_v2.policy = MultiAgentV2Policy::Proactive;
}

fn configure_ultra(config: &mut Config) {
    configure_multi_agent_v2(config);
    config.model_reasoning_effort = Some(ReasoningEffort::Ultra);
}

fn developer_texts(input: &[Value]) -> Vec<&str> {
    input
        .iter()
        .filter(|item| item.get("role").and_then(Value::as_str) == Some("developer"))
        .filter_map(|item| item.get("content")?.as_array())
        .flatten()
        .filter_map(|content| content.get("text")?.as_str())
        .collect()
}

fn count_containing(texts: &[&str], target: &str) -> usize {
    texts.iter().filter(|text| text.contains(target)).count()
}

async fn submit_turn(
    codex: &codex_core::CodexThread,
    prompt: &str,
    effort: Option<ReasoningEffort>,
) -> Result<()> {
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: ThreadSettingsOverrides {
                effort: effort.map(Some),
                ..Default::default()
            },
        })
        .await?;
    wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ultra_reasoning_uses_max_without_changing_the_default_policy() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(configure_ultra)
        .build(&server)
        .await?;

    submit_turn(&test.codex, "hello", /*effort*/ None).await?;

    let request = response.single_request();
    assert_eq!(
        request.body_json()["reasoning"]["effort"].as_str(),
        Some("max")
    );
    let input = request.input();
    let texts = developer_texts(&input);
    assert_eq!(
        (
            count_containing(&texts, NO_SPAWN_TEXT),
            count_containing(&texts, PROACTIVE_TEXT),
        ),
        (1, 0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_explanation_keeps_explicit_policy_across_reasoning_efforts() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        (1..=2)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(configure_mode_explanation)
        .build(&server)
        .await?;
    submit_turn(&test.codex, "explicit", Some(ReasoningEffort::High)).await?;
    submit_turn(&test.codex, "proactive", Some(ReasoningEffort::Ultra)).await?;

    let requests = responses.requests();
    let first_input = requests[0].input();
    let first_texts = developer_texts(&first_input);
    let second_input = requests[1].input();
    let second_texts = developer_texts(&second_input);
    let instruction_counts = |texts: &[&str]| {
        (
            count_containing(texts, MODE_EXPLANATION_TEXT),
            count_containing(texts, NO_SPAWN_TEXT),
            count_containing(texts, PROACTIVE_TEXT),
        )
    };
    assert_eq!(instruction_counts(&first_texts), (1, 1, 0));
    assert_eq!(instruction_counts(&second_texts), (1, 1, 0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_policy_is_independent_of_reasoning_effort() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        (1..=2)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(configure_proactive)
        .build(&server)
        .await?;

    submit_turn(&test.codex, "high", Some(ReasoningEffort::High)).await?;
    submit_turn(&test.codex, "ultra", Some(ReasoningEffort::Ultra)).await?;

    for request in responses.requests() {
        let input = request.input();
        let texts = developer_texts(&input);
        assert_eq!(
            (
                count_containing(&texts, NO_SPAWN_TEXT),
                count_containing(&texts, PROACTIVE_TEXT),
            ),
            (0, 1)
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_configured_explanation_still_emits_the_selected_policy() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            configure_multi_agent_v2(config);
            config.multi_agent_v2.multi_agent_mode_hint_text = Some(String::new());
        })
        .build(&server)
        .await?;

    submit_turn(&test.codex, "hello", Some(ReasoningEffort::High)).await?;

    let input = response.single_request().input();
    let texts = developer_texts(&input);
    assert_eq!(
        (
            count_containing(&texts, MULTI_AGENT_MODE_OPEN_TAG),
            count_containing(&texts, NO_SPAWN_TEXT),
            count_containing(&texts, PROACTIVE_TEXT),
        ),
        (1, 1, 0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_configured_explanation_emits_a_policy_update() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        (1..=2)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;
    let initial = test_codex()
        .with_config(configure_mode_explanation)
        .build(&server)
        .await?;
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    submit_turn(&initial.codex, "before resume", /*effort*/ None).await?;
    drop(initial);

    let mut resume_builder = test_codex().with_config(|config| {
        configure_multi_agent_v2(config);
        config.multi_agent_v2.multi_agent_mode_hint_text = Some(String::new());
    });
    let resumed = resume_builder.resume(&server, home, rollout_path).await?;
    submit_turn(&resumed.codex, "after resume", /*effort*/ None).await?;

    let requests = responses.requests();
    let first_input = requests[0].input();
    let first_texts = developer_texts(&first_input);
    let resumed_input = requests[1].input();
    let resumed_texts = developer_texts(&resumed_input);
    assert_eq!(
        (
            count_containing(&first_texts, MULTI_AGENT_MODE_OPEN_TAG),
            count_containing(&resumed_texts, MULTI_AGENT_MODE_OPEN_TAG),
            count_containing(&first_texts, NO_SPAWN_TEXT),
            count_containing(&resumed_texts, MODE_EXPLANATION_TEXT),
            count_containing(&resumed_texts, NO_SPAWN_TEXT),
        ),
        (1, 2, 1, 1, 2)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_change_does_not_change_policy_or_reappend_usage_hint() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        (1..=2)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(|config| {
            configure_ultra(config);
            config.multi_agent_v2.root_agent_usage_hint_text =
                Some(ROOT_USAGE_HINT_TEXT.to_string());
        })
        .build_with_auto_env(&server)
        .await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    submit_turn(&test.codex, "ultra", /*effort*/ None).await?;
    submit_turn(&test.codex, "high", Some(ReasoningEffort::High)).await?;

    let requests = responses.requests();
    let first_input = requests[0].input();
    let first_texts = developer_texts(&first_input);
    let hint_index = first_texts
        .iter()
        .position(|text| text.contains(ROOT_USAGE_HINT_TEXT))
        .expect("initial usage hint");
    let mode_index = first_texts
        .iter()
        .position(|text| text.contains(NO_SPAWN_TEXT))
        .expect("initial explicit-request-only policy");
    assert!(hint_index < mode_index);

    let second_input = requests[1].input();
    let second_texts = developer_texts(&second_input);
    assert_eq!(
        (
            count_containing(&second_texts, ROOT_USAGE_HINT_TEXT),
            count_containing(&second_texts, PROACTIVE_TEXT),
            count_containing(&second_texts, NO_SPAWN_TEXT),
        ),
        (1, 0, 1),
    );
    test.codex.ensure_rollout_materialized().await;
    test.codex.flush_rollout().await?;
    let rollout_values = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let recorded_modes = rollout_values
        .iter()
        .filter(|value| value.get("type").and_then(Value::as_str) == Some("world_state"))
        .filter_map(|value| {
            value
                .pointer("/payload/state/multi_agent_mode/mode")
                .cloned()
        })
        .collect::<Vec<_>>();
    assert_eq!(recorded_modes, [json!("explicitRequestOnly")]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_change_after_cold_resume_emits_explicit_mode() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        (1..=2)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;
    let initial = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(|config| {
            configure_proactive(config);
            config.model_reasoning_effort = Some(ReasoningEffort::Ultra);
        })
        .build(&server)
        .await?;
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    submit_turn(&initial.codex, "before resume", /*effort*/ None).await?;
    drop(initial);

    let mut resume_builder = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(configure_multi_agent_v2);
    let resumed = resume_builder.resume(&server, home, rollout_path).await?;
    submit_turn(&resumed.codex, "after resume", Some(ReasoningEffort::High)).await?;

    let requests = responses.requests();
    assert_eq!(
        (
            requests[0].body_json()["reasoning"]["effort"]
                .as_str()
                .map(str::to_string),
            requests[1].body_json()["reasoning"]["effort"]
                .as_str()
                .map(str::to_string),
        ),
        (Some("max".to_string()), Some("high".to_string()))
    );
    let resumed_input = requests[1].input();
    let texts = developer_texts(&resumed_input);
    assert_eq!(
        (
            count_containing(&texts, MULTI_AGENT_MODE_OPEN_TAG),
            count_containing(&texts, NO_SPAWN_TEXT),
            count_containing(&texts, PROACTIVE_TEXT),
        ),
        (2, 1, 1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ultra_on_multi_agent_v1_uses_max_without_mode_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", add_ultra_reasoning)
        .with_config(|config| {
            config.model_reasoning_effort = Some(ReasoningEffort::Ultra);
        })
        .build(&server)
        .await?;

    submit_turn(&test.codex, "hello", /*effort*/ None).await?;

    let request = response.single_request();
    assert_eq!(
        request.body_json()["reasoning"]["effort"].as_str(),
        Some("max")
    );
    let input = request.input();
    let texts = developer_texts(&input);
    assert_eq!(count_containing(&texts, MULTI_AGENT_MODE_OPEN_TAG), 0);

    Ok(())
}
