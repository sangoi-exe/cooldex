use super::support::function_spec;
use super::support::user_message;
use codex_api::ResponseEvent;
use codex_cursor_agent_service::COOLDEX_BASE_INSTRUCTIONS_RULE_PATH;
use codex_cursor_agent_service::CursorMappingError;
use codex_cursor_agent_service::CursorSamplingRequest;
use codex_cursor_agent_service::build_request_context;
use codex_cursor_agent_service::map_interaction_update;
use codex_cursor_agent_service::map_request_context_result;
use codex_cursor_agent_service::map_sampling_request;
use codex_cursor_agent_service::proto::CursorRuleSource;
use codex_cursor_agent_service::proto::HeartbeatUpdate;
use codex_cursor_agent_service::proto::InteractionUpdate;
use codex_cursor_agent_service::proto::RequestContextArgs;
use codex_cursor_agent_service::proto::TextDeltaUpdate;
use codex_cursor_agent_service::proto::TurnEndedUpdate;
use codex_cursor_agent_service::proto::exec_client_message;
use codex_cursor_agent_service::proto::interaction_update;
use codex_cursor_agent_service::proto::request_context_result;
use pretty_assertions::assert_eq;

const DECLARED_CONTEXT_REJECTIONS: &[&str] = &[
    "notes_session_id",
    "plugin_cache_root",
    "pinned_tree_sha",
    "use_cached",
    "workspace_id",
];

#[test]
fn maps_minimal_request_with_one_required_cooldex_rule_and_no_cursor_harness() {
    let input = vec![user_message("Do the work")];
    let tools = vec![function_spec("echo")];
    let mapped = map_sampling_request(CursorSamplingRequest {
        conversation_id: "conversation-local",
        model_id: "composer-2.5",
        model_display_name: "Composer 2.5",
        base_instructions: "exact Cooldex harness",
        input: &input,
        tools: &tools,
        current_message_id: "message-current",
        synthesized_user_message: None,
    })
    .expect("minimal representable request should map");

    let request = mapped.request;
    let state = request
        .conversation_state
        .expect("request should include conversation state");
    assert!(state.turns.is_empty());
    assert!(state.root_prompt_messages_json.is_empty());
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(state.summary, None);
    assert_eq!(state.plan, None);

    let action = request
        .action
        .and_then(|action| action.action)
        .expect("request should include a user action");
    let codex_cursor_agent_service::proto::conversation_action::Action::UserMessageAction(action) =
        action;
    assert_eq!(
        action
            .user_message
            .expect("current user should be present")
            .text,
        "Do the work"
    );
    assert!(action.prepend_user_messages.is_empty());
    let context = action
        .request_context
        .expect("current action should include context");
    assert_eq!(context, build_request_context("exact Cooldex harness"));

    assert_eq!(
        request.conversation_id.as_deref(),
        Some("conversation-local")
    );
    assert_eq!(
        request
            .mcp_tools
            .expect("tools should be present")
            .mcp_tools
            .len(),
        1
    );
    assert_eq!(request.mcp_file_system_options, None);
    assert_eq!(request.skill_options, None);
    assert_eq!(request.custom_system_prompt, None);
    assert_eq!(request.exclude_workspace_context, None);
    assert_eq!(request.harness, None);
    assert_eq!(request.suggest_next_prompt, None);
    assert_eq!(request.subagent_type_name, None);
}

#[test]
fn request_context_contains_only_the_exact_required_global_rule() {
    let context = build_request_context("byte-exact instructions\n");

    assert_eq!(context.rules.len(), 1);
    let rule = &context.rules[0];
    assert_eq!(rule.full_path, COOLDEX_BASE_INSTRUCTIONS_RULE_PATH);
    assert_eq!(rule.content, "byte-exact instructions\n");
    assert_eq!(rule.source, CursorRuleSource::User as i32);
    assert_eq!(rule.is_required, Some(true));
    assert!(rule.environments.is_empty());
    assert!(rule.disabled_environments.is_empty());
    assert!(rule.scoped_to.is_empty());
    assert_eq!(context.web_search_enabled, Some(false));
    assert_eq!(context.web_fetch_enabled, Some(false));
    assert_eq!(context.supports_mcp_auth, Some(false));
    assert_eq!(context.skill_options, None);
    assert_eq!(context.mcp_file_system_options, None);
    assert_eq!(context.mcp_info_complete, Some(true));
    assert_eq!(context.rules_info_complete, Some(true));
    assert_eq!(context.env_info_complete, Some(true));
    assert_eq!(context.repository_info_complete, Some(true));
    assert_eq!(context.custom_subagents_info_complete, Some(true));
    assert_eq!(context.agent_skills_info_complete, Some(true));
    assert_eq!(context.mcp_file_system_info_complete, Some(true));
    assert_eq!(context.git_status_info_complete, Some(true));
    assert_eq!(context.search_conversations_enabled, Some(false));
}

#[test]
fn rejects_every_declared_workspace_context_request_in_isolation() {
    let mut tested = Vec::new();
    for case in DECLARED_CONTEXT_REJECTIONS {
        let mut args = RequestContextArgs {
            notes_session_id: None,
            workspace_id: None,
            read_only_pinned_tree_sha: None,
            read_only_plugin_cache_root: None,
            use_cached: None,
        };
        match *case {
            "notes_session_id" => args.notes_session_id = Some("notes".to_string()),
            "plugin_cache_root" => args.read_only_plugin_cache_root = Some("/cache".to_string()),
            "pinned_tree_sha" => args.read_only_pinned_tree_sha = Some("tree".to_string()),
            "use_cached" => args.use_cached = Some(true),
            "workspace_id" => args.workspace_id = Some("workspace".to_string()),
            unknown => panic!("undeclared context rejection case: {unknown}"),
        }

        assert_eq!(
            map_request_context_result(1, "exec".to_string(), &args, "instructions")
                .expect_err("workspace-bearing request should reject"),
            CursorMappingError::WorkspaceContextRequested
        );
        tested.push(*case);
    }

    assert_eq!(tested, DECLARED_CONTEXT_REJECTIONS);
}

#[test]
fn answers_an_empty_request_context_query_without_disk_cache() {
    let message = map_request_context_result(
        7,
        "exec-7".to_string(),
        &RequestContextArgs {
            notes_session_id: None,
            workspace_id: None,
            read_only_pinned_tree_sha: None,
            read_only_plugin_cache_root: None,
            use_cached: Some(false),
        },
        "instructions",
    )
    .expect("empty request context query should succeed");

    assert_eq!(message.id, 7);
    assert_eq!(message.exec_id, "exec-7");
    let Some(exec_client_message::Message::RequestContextResult(result)) = message.message else {
        panic!("expected request-context result");
    };
    let Some(request_context_result::Result::Success(success)) = result.result else {
        panic!("expected request-context success");
    };
    assert_eq!(success.served_from_disk_cache, Some(false));
    assert_eq!(
        success.request_context,
        Some(build_request_context("instructions"))
    );
}

#[test]
fn maps_text_heartbeat_and_terminal_usage_into_existing_response_events() {
    let text = map_interaction_update(
        "run-local",
        &InteractionUpdate {
            message: Some(interaction_update::Message::TextDelta(TextDeltaUpdate {
                text: "hello".to_string(),
            })),
        },
    )
    .expect("text delta should map");
    assert!(matches!(text, Some(ResponseEvent::OutputTextDelta(delta)) if delta == "hello"));

    let heartbeat = map_interaction_update(
        "run-local",
        &InteractionUpdate {
            message: Some(interaction_update::Message::Heartbeat(HeartbeatUpdate {})),
        },
    )
    .expect("heartbeat should be ignored");
    assert!(heartbeat.is_none());

    let completed = map_interaction_update(
        "run-local",
        &InteractionUpdate {
            message: Some(interaction_update::Message::TurnEnded(TurnEndedUpdate {
                input_tokens: Some(10),
                output_tokens: Some(4),
                cache_read_tokens: Some(3),
                cache_write_tokens: Some(2),
                reasoning_tokens: Some(1),
            })),
        },
    )
    .expect("terminal should map");
    let Some(ResponseEvent::Completed {
        response_id,
        token_usage,
        end_turn,
    }) = completed
    else {
        panic!("expected completed event");
    };
    assert_eq!(response_id, "run-local");
    assert_eq!(end_turn, Some(true));
    let usage = token_usage.expect("usage should be present");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.cached_input_tokens, 3);
    assert_eq!(usage.cache_write_input_tokens, 2);
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(usage.reasoning_output_tokens, 1);
    assert_eq!(usage.total_tokens, 14);
}

#[test]
fn rejects_unknown_or_nonterminal_interaction_updates() {
    let error = map_interaction_update("run-local", &InteractionUpdate { message: None })
        .expect_err("unset interaction update should reject");

    assert_eq!(error, CursorMappingError::UnsupportedInteractionUpdate);
}
