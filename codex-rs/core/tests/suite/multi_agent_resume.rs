use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::TurnStartOptions;
use codex_core::config::AgentRoleConfig;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::assert_parent_turn;
use core_test_support::responses::assert_root_turn;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio::time::timeout;

#[path = "multi_agent_restore_tests.rs"]
mod restore_tests;

const COLLABORATION_NAMESPACE: &str = "collaboration";
const SPAWN_CALL_ID: &str = "spawn-worker";
const NESTED_CALL_ID: &str = "spawn-grandchild";
const QUEUE_CALL_ID: &str = "queue-worker-message";
const FOLLOWUP_CALL_ID: &str = "followup-worker";
const SIBLING_SPAWN_CALL_ID: &str = "spawn-survivor";
const SIBLING_FOLLOWUP_CALL_ID: &str = "followup-survivor";
const INTERRUPT_CALL_ID: &str = "interrupt-worker";
const INITIAL_PROMPT: &str = "spawn a durable worker";
const INITIAL_TASK: &str = "inspect the repository";
const NESTED_TASK: &str = "inspect the nested repository";
const QUEUE_PROMPT: &str = "queue context for the durable worker";
const QUEUED_MESSAGE: &str = "queue-only context from an earlier parent turn";
const FOLLOWUP_PROMPT: &str = "continue the durable worker";
const FOLLOWUP_TASK: &str = "inspect the tests too";
const SIBLING_PROMPT: &str = "spawn a second durable worker";
const SIBLING_TASK: &str = "inspect the release lifecycle";
const SIBLING_FOLLOWUP_PROMPT: &str = "continue the surviving worker";
const SIBLING_FOLLOWUP_TASK: &str = "verify the surviving worker";
const INTERRUPT_PROMPT: &str = "release the interrupted worker";
const TERMINAL_TRIGGER_MESSAGE: &str = "trigger mail after terminal delivery";
const TERMINAL_TRIGGER_PARENT_TURN: &str = "terminal-trigger-parent";
const TERMINAL_TRIGGER_ROOT_TURN: &str = "terminal-trigger-root";
const SIBLING_NAME: &str = "survivor";
const ROLE_NAME: &str = "durable_worker";
const ROLE_MODEL: &str = "gpt-5.6-sol";
const ROLE_MODEL_PROVIDER_ID: &str = "openai";
const ROLE_DEVELOPER_INSTRUCTIONS: &str = "Keep the durable worker role configuration.";
const MUTATED_ROLE_DEVELOPER_INSTRUCTIONS: &str = "Use mutated role instructions.";
const SUBAGENT_DEVELOPER_INSTRUCTIONS: &str = "Use the default durable worker instructions.";

fn decoded_body(request: &wiremock::Request) -> Option<Vec<u8>> {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
    } else {
        Some(request.body.clone())
    }
}

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    decoded_body(request)
        .and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

fn request_has_model(request: &wiremock::Request, model: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .is_some_and(|body| body.get("model").and_then(Value::as_str) == Some(model))
}

fn request_has_input_type(request: &wiremock::Request, input_type: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .and_then(|body| body.get("input").and_then(Value::as_array).cloned())
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some(input_type))
        })
}

fn value_contains_text(value: &Value, text: &str) -> bool {
    match value {
        Value::String(value) => value.contains(text),
        Value::Array(values) => values.iter().any(|value| value_contains_text(value, text)),
        Value::Object(values) => values
            .values()
            .any(|value| value_contains_text(value, text)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

struct GatedSseResponder {
    response: wiremock::ResponseTemplate,
    request_body: Arc<Mutex<Option<Value>>>,
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl wiremock::Respond for GatedSseResponder {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let request_body = decoded_body(request)
            .and_then(|body| serde_json::from_slice(&body).ok())
            .expect("child compaction request should be JSON");
        *self
            .request_body
            .lock()
            .expect("child compaction request body lock") = Some(request_body);
        self.entered
            .send(())
            .expect("child compaction should enter the response gate");
        self.release
            .lock()
            .expect("child compaction response gate lock")
            .take()
            .expect("child compaction response gate should release once")
            .recv()
            .expect("child compaction response should be released");
        self.response.clone()
    }
}

async fn mount_root_collaboration_call(
    server: &wiremock::MockServer,
    prompt: &'static str,
    call_id: &'static str,
    tool_name: &'static str,
    arguments: &str,
) {
    let first_response_id = format!("resp-{call_id}-1");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, prompt) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created(&first_response_id),
            ev_function_call_with_namespace(call_id, COLLABORATION_NAMESPACE, tool_name, arguments),
            ev_completed(&first_response_id),
        ]),
    )
    .await;

    let second_response_id = format!("resp-{call_id}-2");
    let message_id = format!("msg-{call_id}-2");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, call_id) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created(&second_response_id),
            ev_assistant_message(&message_id, "collaboration completed"),
            ev_completed(&second_response_id),
        ]),
    )
    .await;
}

fn configure_multi_agent_v2_with_role(
    config: &mut codex_core::config::Config,
    model_provider_base_url: &str,
) {
    config
        .features
        .enable(Feature::Collab)
        .expect("test config should allow feature update");
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.subagent_developer_instructions =
        Some(SUBAGENT_DEVELOPER_INSTRUCTIONS.to_string());
    config.multi_agent_v2.max_concurrent_threads_per_session = 3;
    let role_path = config.codex_home.join("durable-worker-role.toml");
    std::fs::write(
        &role_path,
        format!(
            "model = \"{ROLE_MODEL}\"\nmodel_reasoning_effort = \"high\"\ndeveloper_instructions = \"{ROLE_DEVELOPER_INSTRUCTIONS}\"\nsandbox_mode = \"read-only\"\nmodel_provider = \"mock\"\n\n[model_providers.mock]\nname = \"mock\"\nbase_url = \"{model_provider_base_url}\"\nenv_key = \"PATH\"\nwire_api = \"responses\"\n"
        ),
    )
    .expect("write durable worker role config");
    config.agent_roles.insert(
        ROLE_NAME.to_string(),
        AgentRoleConfig {
            description: Some("Durable worker role".to_string()),
            config_file: Some(role_path.to_path_buf()),
            nickname_candidates: None,
        },
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_root_resume_restores_agent_identity_after_real_child_compaction() -> Result<()> {
    let server = start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": INITIAL_TASK,
        "task_name": "worker",
        "agent_type": ROLE_NAME,
        "fork_turns": "none",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, INITIAL_PROMPT),
        sse(vec![
            ev_response_created("resp-spawn-1"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            ev_completed("resp-spawn-1"),
        ]),
    )
    .await;
    let initial_child_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, INITIAL_TASK)
        },
        sse(vec![
            ev_response_created("resp-worker-1"),
            ev_function_call_with_namespace(
                NESTED_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                r#"{"message":"inspect the nested repository","task_name":"grandchild","fork_turns":"none"}"#,
            ),
            ev_completed("resp-worker-1"),
        ]),
    )
    .await;
    let nested_mock = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, NESTED_TASK)
                && request_has_input_type(request, "agent_message")
                && !body_contains(request, NESTED_CALL_ID)
        },
        sse(vec![ev_completed("resp-parent-turn-assistant")]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL) && body_contains(request, "later child user")
        },
        sse(vec![ev_completed("resp-worker-later-user")]),
    )
    .await;
    // The grandchild's completion can arrive during the worker's completion response,
    // causing one more sampling request to drain that message before the turn ends.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .and(|request: &wiremock::Request| {
            !body_contains(request, FOLLOWUP_TASK)
                && !request_has_input_type(request, "compaction_trigger")
                && decoded_body(request)
                    .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                    .is_some_and(|body| {
                        body["input"].as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item["type"] == "function_call_output"
                                    && item["call_id"] == NESTED_CALL_ID
                            })
                        })
                    })
        })
        .respond_with(sse_response(sse(vec![ev_completed(
            "resp-worker-complete",
        )])))
        .expect(1..=2)
        .mount(&server)
        .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, QUEUE_CALL_ID)
                && !request_has_input_type(request, "agent_message")
        },
        sse(vec![ev_completed("resp-parent-turn-assistant")]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SPAWN_CALL_ID) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created("resp-spawn-2"),
            ev_assistant_message("msg-spawn-2", "worker spawned"),
            ev_completed("resp-spawn-2"),
        ]),
    )
    .await;

    let initial_model_provider_base_url = format!("{}/v1", server.uri());
    let mut initial_builder = test_codex().with_config(move |config| {
        configure_multi_agent_v2_with_role(config, &initial_model_provider_base_url);
    });
    let initial = initial_builder.build_with_auto_env(&server).await?;
    let root_thread_id = initial.session_configured.thread_id;
    initial
        .codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: INITIAL_PROMPT.to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                permission_profile: Some(PermissionProfile::Disabled),
                ..Default::default()
            }),
        )
        .await?;
    wait_for_event(&initial.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let deadline = Instant::now() + Duration::from_secs(2);
    let worker_thread_id = loop {
        if let Some(thread_id) = initial_child_request
            .requests()
            .into_iter()
            .find_map(|request| {
                let body = request.body_json();
                if body["client_metadata"]["x-codex-parent-thread-id"] != json!(root_thread_id) {
                    return None;
                }
                body["client_metadata"]["thread_id"]
                    .as_str()
                    .and_then(|thread_id| codex_protocol::ThreadId::from_string(thread_id).ok())
            })
        {
            break thread_id;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for spawned worker");
        }
        sleep(Duration::from_millis(10)).await;
    };
    let worker_thread = initial.thread_manager.get_thread(worker_thread_id).await?;
    wait_for_event(worker_thread.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(initial_child_request.requests().iter().any(|request| {
        request.body_contains_text(INITIAL_TASK)
            && request.body_contains_text(ROLE_DEVELOPER_INSTRUCTIONS)
            && request.body_contains_text("<permission_profile type=\"disabled\">")
            && !request.body_contains_text(SUBAGENT_DEVELOPER_INSTRUCTIONS)
    }));
    assert_eq!(
        worker_thread.config().await.model_provider,
        initial.codex.config().await.model_provider,
        "roles must inherit the parent's complete model provider",
    );
    let initial_worker_config = worker_thread.config_snapshot().await;
    let initial_worker_role_config = (
        initial_worker_config.model,
        initial_worker_config.model_provider_id,
        initial_worker_config.reasoning_effort,
        initial_worker_config.permission_profile,
    );
    assert_eq!(
        initial_worker_role_config,
        (
            ROLE_MODEL.to_string(),
            ROLE_MODEL_PROVIDER_ID.to_string(),
            Some(ReasoningEffort::High),
            PermissionProfile::Disabled,
        )
    );
    // The real paginated compactor must remain admissible when its consumer submits it immediately
    // after observing the preceding TurnComplete.
    let (compaction_entered, compaction_entered_receiver) = mpsc::channel();
    let (release_compaction, compaction_release_receiver) = mpsc::channel();
    let child_compaction_body = Arc::new(Mutex::new(None));
    let child_thread_id_for_compaction = worker_thread_id;
    let child_compaction_body_for_response = Arc::clone(&child_compaction_body);
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .and(move |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "compaction_trigger")
                && request.body_json::<Value>().is_ok_and(|body| {
                    body["client_metadata"]["thread_id"] == json!(child_thread_id_for_compaction)
                })
        })
        .respond_with(GatedSseResponder {
            response: sse_response(sse(vec![
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "compaction",
                        "encrypted_content": "DURABLE_CHILD_COMPACTION_SUMMARY",
                    }
                }),
                ev_completed("resp-worker-compact"),
            ])),
            request_body: child_compaction_body_for_response,
            entered: compaction_entered,
            release: Mutex::new(Some(compaction_release_receiver)),
        })
        .expect(1)
        .mount(&server)
        .await;
    let child_thread_id_for_terminal_trigger = worker_thread_id;
    let terminal_trigger_request = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, TERMINAL_TRIGGER_MESSAGE)
                && request.body_json::<Value>().is_ok_and(|body| {
                    body["client_metadata"]["thread_id"]
                        == json!(child_thread_id_for_terminal_trigger)
                })
        },
        sse(vec![ev_completed("resp-worker-terminal-trigger")]),
    )
    .await;
    worker_thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "later child user".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(worker_thread.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    worker_thread.submit(Op::Compact).await?;
    let compaction_entry = timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || compaction_entered_receiver.recv()),
    )
    .await
    .context("timed out waiting for child compaction response gate")??;
    compaction_entry.context("child compaction response gate closed before admission")?;
    worker_thread
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::root(),
                AgentPath::root().join("worker").expect("valid worker path"),
                Vec::new(),
                TERMINAL_TRIGGER_MESSAGE.to_string(),
                /*trigger_turn*/ true,
            ),
            start_options: TurnStartOptions {
                parent_turn_id: Some(TERMINAL_TRIGGER_PARENT_TURN.to_string()),
                root_turn_id: Some(TERMINAL_TRIGGER_ROOT_TURN.to_string()),
                ..Default::default()
            },
        })
        .await?;
    assert!(
        terminal_trigger_request.requests().is_empty(),
        "trigger mail must remain queued while the child compaction response is held",
    );
    release_compaction
        .send(())
        .expect("release child compaction response");
    let EventMsg::TurnComplete(compaction_completed) =
        wait_for_event(worker_thread.as_ref(), |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await
    else {
        unreachable!("event predicate guarantees compaction completion")
    };
    let EventMsg::TurnComplete(terminal_trigger_completed) =
        wait_for_event(worker_thread.as_ref(), |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await
    else {
        unreachable!("event predicate guarantees terminal trigger completion")
    };
    assert_ne!(
        compaction_completed.turn_id,
        terminal_trigger_completed.turn_id
    );
    assert!(
        !child_compaction_body
            .lock()
            .expect("child compaction request body lock")
            .as_ref()
            .expect("captured child compaction request body")
            .to_string()
            .contains(TERMINAL_TRIGGER_MESSAGE)
    );
    let terminal_trigger_body = terminal_trigger_request.single_request().body_json();
    assert_eq!(
        terminal_trigger_body["input"]
            .as_array()
            .expect("terminal trigger request input")
            .iter()
            .filter(|item| {
                item["type"] == "agent_message"
                    && item["content"].as_array().is_some_and(|content| {
                        content.iter().any(|content| {
                            content["type"] == "input_text"
                                && content["text"] == TERMINAL_TRIGGER_MESSAGE
                        })
                    })
            })
            .count(),
        1
    );
    assert_parent_turn(&terminal_trigger_body, Some(TERMINAL_TRIGGER_PARENT_TURN))?;
    assert_root_turn(&terminal_trigger_body, Some(TERMINAL_TRIGGER_ROOT_TURN))?;

    // Merge-safety anchor: flush the worker while it is still resident before sibling
    // creation can evict it at the configured thread capacity; retain the later sibling/root
    // durability checks and idempotent shutdown without adding recovery behavior.
    worker_thread.flush_rollout().await.with_context(|| {
        format!("failed to flush worker thread {worker_thread_id} rollout before sibling spawn")
    })?;
    let worker_history = worker_thread
        .load_history(/*include_archived*/ false)
        .await?;
    let persisted_developer_instructions = worker_history
        .items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event)) => {
                Some(event.thread_settings.developer_instructions.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        persisted_developer_instructions.last(),
        Some(&Some(Some(ROLE_DEVELOPER_INSTRUCTIONS.to_string()))),
        "the latest worker settings snapshot must preserve the role developer instructions"
    );
    let compaction_completion_index = worker_history
        .items
        .iter()
        .position(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::TurnComplete(event))
                    if event.turn_id == compaction_completed.turn_id
            )
        })
        .expect("persisted compact completion");
    let terminal_trigger_indices = worker_history
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            matches!(
                item,
                RolloutItem::ResponseItem(envelope)
                    if matches!(
                        &envelope.item,
                        ResponseItem::AgentMessage { content, .. }
                            if content.iter().any(|item| matches!(
                                item,
                                AgentMessageInputContent::InputText { text }
                                    if text == TERMINAL_TRIGGER_MESSAGE
                            ))
                    )
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_trigger_indices.len(), 1);
    assert!(terminal_trigger_indices[0] > compaction_completion_index);

    let sibling_spawn_args = serde_json::to_string(&json!({
        "message": SIBLING_TASK,
        "task_name": SIBLING_NAME,
        "agent_type": ROLE_NAME,
        "fork_turns": "none",
    }))?;
    mount_root_collaboration_call(
        &server,
        SIBLING_PROMPT,
        SIBLING_SPAWN_CALL_ID,
        "spawn_agent",
        &sibling_spawn_args,
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, SIBLING_TASK)
        },
        sse(vec![
            ev_response_created("resp-survivor-1"),
            ev_assistant_message("msg-survivor-1", "initial survivor task complete"),
            ev_completed("resp-survivor-1"),
        ]),
    )
    .await;
    initial.submit_turn(SIBLING_PROMPT).await?;

    let grandchild = nested_mock.last_request().expect("grandchild").body_json();
    let nested_id = &grandchild["client_metadata"]["thread_id"];
    let sibling_thread_id = initial
        .thread_manager
        .list_thread_ids()
        .await
        .into_iter()
        .find(|id| ![root_thread_id, worker_thread_id].contains(id) && &json!(id) != nested_id)
        .ok_or_else(|| anyhow::anyhow!("spawned sibling should be registered"))?;
    let sibling_thread = initial.thread_manager.get_thread(sibling_thread_id).await?;
    wait_for_event(sibling_thread.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    sibling_thread.flush_rollout().await.with_context(|| {
        format!("failed to flush sibling thread {sibling_thread_id} rollout before restart")
    })?;
    initial.codex.flush_rollout().await.with_context(|| {
        format!("failed to flush root thread {root_thread_id} rollout before restart")
    })?;
    sibling_thread.shutdown_and_wait().await?;
    worker_thread.shutdown_and_wait().await?;
    drop(sibling_thread);
    drop(worker_thread);

    let followup_args = serde_json::to_string(&json!({
        "target": "worker",
        "message": FOLLOWUP_TASK,
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, FOLLOWUP_PROMPT),
        sse(vec![
            ev_response_created("resp-followup-1"),
            ev_function_call_with_namespace(
                FOLLOWUP_CALL_ID,
                COLLABORATION_NAMESPACE,
                "followup_task",
                &followup_args,
            ),
            ev_completed("resp-followup-1"),
        ]),
    )
    .await;
    let (followup_entered, followup_entered_receiver) = mpsc::channel();
    let (release_followup, followup_release_receiver) = mpsc::channel();
    let followup_child_thread_id = worker_thread_id;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .and(move |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, FOLLOWUP_TASK)
                && body_contains(request, QUEUED_MESSAGE)
                && decoded_body(request)
                    .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                    .is_some_and(|body| {
                        body["client_metadata"]["thread_id"] == json!(followup_child_thread_id)
                    })
        })
        .respond_with(GatedSseResponder {
            response: sse_response(sse(vec![
                ev_response_created("resp-worker-2"),
                ev_assistant_message("msg-worker-2", "follow-up complete"),
                ev_completed("resp-worker-2"),
            ])),
            request_body: Arc::new(Mutex::new(None)),
            entered: followup_entered,
            release: Mutex::new(Some(followup_release_receiver)),
        })
        .expect(1)
        .mount(&server)
        .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FOLLOWUP_CALL_ID) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created("resp-followup-2"),
            ev_assistant_message("msg-followup-2", "follow-up sent"),
            ev_completed("resp-followup-2"),
        ]),
    )
    .await;

    let resumed_model_provider_base_url = format!("{}/v1", server.uri());
    let mut resume_builder = test_codex().with_config(move |config| {
        configure_multi_agent_v2_with_role(config, &resumed_model_provider_base_url);
    });
    let resumed = resume_builder.restart(&server, &initial).await?;
    drop(initial);
    assert_eq!(
        resumed.thread_manager.list_thread_ids().await,
        vec![root_thread_id]
    );
    assert!(
        resumed
            .thread_manager
            .get_thread(worker_thread_id)
            .await
            .is_err()
    );
    assert!(
        resumed
            .thread_manager
            .get_thread(sibling_thread_id)
            .await
            .is_err()
    );

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| request_has_input_type(request, "compaction_trigger"),
        sse(vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "DURABLE_AGENT_COMPACTION_SUMMARY",
                }
            }),
            ev_completed("resp-durable-agent-compact"),
        ]),
    )
    .await;
    resumed.codex.submit(Op::Compact).await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let redirected_server = start_mock_server().await;
    let redirected_base_url = format!("{}/v1", redirected_server.uri());
    std::fs::write(
        resumed.config.codex_home.join("durable-worker-role.toml"),
        format!(
            r#"model = "{ROLE_MODEL}"
model_reasoning_effort = "high"
developer_instructions = "{MUTATED_ROLE_DEVELOPER_INSTRUCTIONS}"
model_provider = "{ROLE_MODEL_PROVIDER_ID}"
openai_base_url = "{redirected_base_url}"
"#
        ),
    )?;

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, QUEUE_PROMPT),
        sse(vec![
            ev_response_created("resp-queue"),
            ev_function_call_with_namespace(
                QUEUE_CALL_ID,
                COLLABORATION_NAMESPACE,
                "send_message",
                r#"{"target":"worker","message":"queue-only context from an earlier parent turn"}"#,
            ),
            ev_completed("resp-queue"),
        ]),
    )
    .await;
    resumed.submit_turn(QUEUE_PROMPT).await?;

    let reloaded_worker = resumed
        .thread_manager
        .get_thread(worker_thread_id)
        .await
        .expect("queued message should lazily reload the original worker");
    assert_eq!(
        reloaded_worker.config().await.model_provider,
        resumed.codex.config().await.model_provider,
        "cold reload must preserve the parent's complete model provider",
    );
    assert_eq!(
        reloaded_worker
            .config()
            .await
            .developer_instructions
            .as_deref(),
        Some(ROLE_DEVELOPER_INSTRUCTIONS),
        "cold reload must restore the persisted role developer instructions",
    );
    assert_eq!(
        reloaded_worker
            .config()
            .await
            .permissions
            .permission_profile()
            .clone(),
        PermissionProfile::Disabled,
        "cold reload must restore the persisted birth permission profile",
    );
    let root_followup = resumed.submit_turn(FOLLOWUP_PROMPT);
    tokio::pin!(root_followup);
    let followup_gate = timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || followup_entered_receiver.recv()),
    );
    tokio::pin!(followup_gate);
    let (root_followup_result, followup_gate_result) = tokio::select! {
        result = &mut root_followup => (Some(result), None),
        result = &mut followup_gate => (None, Some(result)),
    };
    let followup_entry = match followup_gate_result {
        Some(result) => result,
        None => followup_gate.await,
    }
    .context("timed out waiting for resumed child follow-up response gate")??;
    followup_entry.context("resumed child follow-up response gate closed before admission")?;
    release_followup
        .send(())
        .expect("release resumed child follow-up response");
    match root_followup_result {
        Some(result) => result?,
        None => root_followup.await?,
    }
    wait_for_event(reloaded_worker.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    // The worker is loaded again, while its alphabetically earlier sibling remains unloaded.
    mount_sse_once(
        &server,
        sse(vec![
            json!({
                "type": "response.output_item.done",
                "item": { "type": "compaction", "encrypted_content": "LOADED_FIRST_COMPACTION" },
            }),
            ev_completed("resp-loaded-first-compact"),
        ]),
    )
    .await;
    resumed.codex.submit(Op::Compact).await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let roster_request =
        mount_sse_once(&server, sse(vec![ev_completed("resp-loaded-first-roster")])).await;
    resumed.submit_turn("inspect the agent roster").await?;
    assert!(roster_request.single_request().body_contains_text(
        r#"<subagents>
    <agent name="/root/worker" />
    <agent name="/root/survivor" />
  </subagents>"#
    ));

    let requests = server
        .received_requests()
        .await
        .expect("captured response requests");
    assert!(!requests.iter().any(|request| {
        decoded_body(request)
            .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
            .is_some_and(|body| body["client_metadata"]["thread_id"] == json!(worker_thread_id))
            && body_contains(request, QUEUED_MESSAGE)
            && !body_contains(request, FOLLOWUP_TASK)
    }));
    let body_for = |text: &str, thread: codex_protocol::ThreadId| {
        requests
            .iter()
            .find_map(|request| {
                let body: Value = serde_json::from_slice(&decoded_body(request)?).ok()?;
                (body_contains(request, text)
                    && body["client_metadata"]["thread_id"] == json!(thread))
                .then_some(body)
            })
            .expect("matching model request for expected thread")
    };
    let initial_root = body_for(INITIAL_PROMPT, root_thread_id);
    let queue_root = body_for(QUEUE_PROMPT, root_thread_id);
    let followup_root = body_for(FOLLOWUP_PROMPT, root_thread_id);
    let initial_child = body_for(INITIAL_TASK, worker_thread_id);
    let followup_child = body_for(FOLLOWUP_TASK, worker_thread_id);
    let followup_has_queue = value_contains_text(&followup_child, QUEUED_MESSAGE);
    let followup_has_original_role =
        value_contains_text(&followup_child, ROLE_DEVELOPER_INSTRUCTIONS);
    let followup_has_birth_permission =
        value_contains_text(&followup_child, "<permission_profile type=\"disabled\">");
    let followup_has_default_role =
        value_contains_text(&followup_child, SUBAGENT_DEVELOPER_INSTRUCTIONS);
    let followup_has_mutated_role =
        value_contains_text(&followup_child, MUTATED_ROLE_DEVELOPER_INSTRUCTIONS);
    assert!(
        followup_has_queue
            && followup_has_original_role
            && followup_has_birth_permission
            && !followup_has_default_role
            && !followup_has_mutated_role,
        "resumed child follow-up identity: queue={followup_has_queue}, original_role={followup_has_original_role}, birth_permission={followup_has_birth_permission}, default_role={followup_has_default_role}, mutated_role={followup_has_mutated_role}"
    );
    let roster = queue_root["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|item| item["text"].as_str())
        .rfind(|text| text.contains("<subagents>"))
        .expect("post-compaction context should include cold agents");
    assert!(roster.contains(r#"<agent name="/root/worker" />"#));
    assert!(roster.contains(r#"<agent name="/root/survivor" />"#));
    assert!(!roster.contains("grandchild"));
    let initial_parent = initial_root["client_metadata"]["turn_id"]
        .as_str()
        .expect("initial parent turn");
    let queue_parent = queue_root["client_metadata"]["turn_id"]
        .as_str()
        .expect("queue-only parent turn");
    let followup_parent = followup_root["client_metadata"]["turn_id"]
        .as_str()
        .expect("follow-up parent turn");
    assert_ne!(followup_parent, initial_parent);
    assert_ne!(followup_parent, queue_parent);
    let nested_parent = initial_child["client_metadata"]["turn_id"]
        .as_str()
        .expect("nested worker parent turn");
    for (body, parent_thread, parent_turn) in [
        (&initial_root, None, None),
        (&queue_root, None, None),
        (&followup_root, None, None),
        (&initial_child, Some(root_thread_id), Some(initial_parent)),
        (&followup_child, Some(root_thread_id), Some(followup_parent)),
        (&grandchild, Some(worker_thread_id), Some(nested_parent)),
    ] {
        if let Some(parent_thread) = parent_thread {
            assert_eq!(
                body["client_metadata"]["x-codex-parent-thread-id"],
                json!(parent_thread)
            );
        }
        assert_parent_turn(body, parent_turn)?;
    }
    for (body, root_turn) in [
        (&initial_root, initial_parent),
        (&queue_root, queue_parent),
        (&followup_root, followup_parent),
        (&initial_child, initial_parent),
        (&followup_child, followup_parent),
        (&grandchild, initial_parent),
    ] {
        assert_root_turn(body, Some(root_turn))?;
    }
    let reloaded_worker_config = reloaded_worker.config_snapshot().await;
    let reloaded_worker_role_config = (
        reloaded_worker_config.model,
        reloaded_worker_config.model_provider_id,
        reloaded_worker_config.reasoning_effort,
        reloaded_worker_config.permission_profile,
    );
    assert_eq!(reloaded_worker_role_config, initial_worker_role_config);

    reloaded_worker.shutdown_and_wait().await?;
    assert!(
        resumed
            .thread_manager
            .get_thread(worker_thread_id)
            .await
            .is_ok()
    );

    let interrupt_args = serde_json::to_string(&json!({
        "target": "worker",
    }))?;
    mount_root_collaboration_call(
        &server,
        INTERRUPT_PROMPT,
        INTERRUPT_CALL_ID,
        "interrupt_agent",
        &interrupt_args,
    )
    .await;
    resumed.submit_turn(INTERRUPT_PROMPT).await?;
    assert!(
        resumed
            .thread_manager
            .get_thread(worker_thread_id)
            .await
            .is_err()
    );

    let sibling_followup_args = serde_json::to_string(&json!({
        "target": SIBLING_NAME,
        "message": SIBLING_FOLLOWUP_TASK,
    }))?;
    mount_root_collaboration_call(
        &server,
        SIBLING_FOLLOWUP_PROMPT,
        SIBLING_FOLLOWUP_CALL_ID,
        "followup_task",
        &sibling_followup_args,
    )
    .await;
    let sibling_followup_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, SIBLING_FOLLOWUP_TASK)
        },
        sse(vec![
            ev_response_created("resp-survivor-2"),
            ev_assistant_message("msg-survivor-2", "survivor follow-up complete"),
            ev_completed("resp-survivor-2"),
        ]),
    )
    .await;
    resumed.submit_turn(SIBLING_FOLLOWUP_PROMPT).await?;

    let surviving_sibling = resumed
        .thread_manager
        .get_thread(sibling_thread_id)
        .await
        .expect("follow-up should reload the surviving sibling");
    wait_for_event(surviving_sibling.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(sibling_followup_request.requests().iter().any(|request| {
        request.body_contains_text(SIBLING_FOLLOWUP_TASK)
            && request.body_contains_text(ROLE_DEVELOPER_INSTRUCTIONS)
    }));
    assert!(
        redirected_server
            .received_requests()
            .await
            .expect("captured redirected-provider requests")
            .is_empty(),
        "a changed role must not redirect resumed model requests",
    );

    Ok(())
}
