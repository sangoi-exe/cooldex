use super::COOLDEX_BASE_INSTRUCTIONS_RULE_PATH;
use super::CursorMappingError;
use super::history::map_conversation;
use super::tools::CursorToolSnapshot;
use super::values::map_token_usage;
use crate::proto::AgentRunRequest;
use crate::proto::ConversationAction;
use crate::proto::CursorRule;
use crate::proto::CursorRuleSource;
use crate::proto::CursorRuleType;
use crate::proto::CursorRuleTypeGlobal;
use crate::proto::ExecClientMessage;
use crate::proto::ModelDetails;
use crate::proto::RequestContext;
use crate::proto::RequestContextArgs;
use crate::proto::RequestContextResult;
use crate::proto::RequestContextSuccess;
use crate::proto::RequestedModel;
use crate::proto::UserMessageAction;
use crate::proto::conversation_action;
use crate::proto::cursor_rule_type;
use crate::proto::exec_client_message;
use crate::proto::interaction_update;
use crate::proto::request_context_result;
use codex_api::ResponseEvent;
use codex_protocol::models::ResponseItem;
use codex_tools::ToolSpec;

#[derive(Clone, Debug)]
pub struct CursorSamplingRequest<'a> {
    pub conversation_id: &'a str,
    pub model_id: &'a str,
    pub model_display_name: &'a str,
    pub base_instructions: &'a str,
    pub input: &'a [ResponseItem],
    pub tools: &'a [ToolSpec],
    pub current_message_id: &'a str,
    pub synthesized_user_message: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct MappedCursorRunRequest {
    pub request: AgentRunRequest,
    pub tool_snapshot: CursorToolSnapshot,
}

pub fn map_sampling_request(
    input: CursorSamplingRequest<'_>,
) -> Result<MappedCursorRunRequest, CursorMappingError> {
    let tool_snapshot = CursorToolSnapshot::from_specs(input.tools)?;
    let (conversation_state, user_message) = map_conversation(
        input.input,
        input.current_message_id,
        input.synthesized_user_message,
        &tool_snapshot,
    )?;
    let request_context = build_request_context(input.base_instructions);
    let request = AgentRunRequest {
        conversation_state: Some(conversation_state),
        action: Some(ConversationAction {
            action: Some(conversation_action::Action::UserMessageAction(
                UserMessageAction {
                    user_message: Some(user_message),
                    request_context: Some(request_context),
                    send_to_interaction_listener: None,
                    prepend_user_messages: Vec::new(),
                },
            )),
        }),
        model_details: Some(ModelDetails {
            model_id: input.model_id.to_string(),
            display_model_id: input.model_id.to_string(),
            display_name: input.model_display_name.to_string(),
            display_name_short: input.model_display_name.to_string(),
            aliases: Vec::new(),
            max_mode: None,
        }),
        mcp_tools: Some(tool_snapshot.mcp_tools()),
        conversation_id: Some(input.conversation_id.to_string()),
        mcp_file_system_options: None,
        skill_options: None,
        custom_system_prompt: None,
        requested_model: Some(RequestedModel {
            model_id: input.model_id.to_string(),
            max_mode: false,
            parameters: Vec::new(),
            built_in_model: true,
            is_variant_string_representation: false,
        }),
        suggest_next_prompt: None,
        subagent_type_name: None,
        exclude_workspace_context: None,
        harness: None,
    };
    Ok(MappedCursorRunRequest {
        request,
        tool_snapshot,
    })
}

pub fn build_request_context(base_instructions: &str) -> RequestContext {
    RequestContext {
        rules: vec![CursorRule {
            full_path: COOLDEX_BASE_INSTRUCTIONS_RULE_PATH.to_string(),
            content: base_instructions.to_string(),
            r#type: Some(CursorRuleType {
                r#type: Some(cursor_rule_type::Type::Global(CursorRuleTypeGlobal {})),
            }),
            source: CursorRuleSource::User as i32,
            environments: Vec::new(),
            disabled_environments: Vec::new(),
            scoped_to: Vec::new(),
            frontmatter: String::new(),
            is_required: Some(true),
        }],
        web_search_enabled: Some(false),
        skill_options: None,
        mcp_file_system_options: None,
        web_fetch_enabled: Some(false),
        supports_mcp_auth: Some(false),
        mcp_info_complete: Some(true),
        rules_info_complete: Some(true),
        env_info_complete: Some(true),
        repository_info_complete: Some(true),
        custom_subagents_info_complete: Some(true),
        agent_skills_info_complete: Some(true),
        mcp_file_system_info_complete: Some(true),
        git_status_info_complete: Some(true),
        search_conversations_enabled: Some(false),
    }
}

pub fn map_request_context_result(
    exec_message_id: u32,
    exec_id: String,
    args: &RequestContextArgs,
    base_instructions: &str,
) -> Result<ExecClientMessage, CursorMappingError> {
    if args.notes_session_id.is_some()
        || args.workspace_id.is_some()
        || args.read_only_pinned_tree_sha.is_some()
        || args.read_only_plugin_cache_root.is_some()
        || args.use_cached == Some(true)
    {
        return Err(CursorMappingError::WorkspaceContextRequested);
    }
    Ok(ExecClientMessage {
        id: exec_message_id,
        exec_id,
        local_execution_time_ms: None,
        message: Some(exec_client_message::Message::RequestContextResult(
            RequestContextResult {
                result: Some(request_context_result::Result::Success(
                    RequestContextSuccess {
                        request_context: Some(build_request_context(base_instructions)),
                        served_from_disk_cache: Some(false),
                    },
                )),
            },
        )),
    })
}

pub fn map_interaction_update(
    response_id: &str,
    update: &crate::proto::InteractionUpdate,
) -> Result<Option<ResponseEvent>, CursorMappingError> {
    match update.message.as_ref() {
        Some(interaction_update::Message::TextDelta(delta)) => {
            Ok(Some(ResponseEvent::OutputTextDelta(delta.text.clone())))
        }
        Some(interaction_update::Message::Heartbeat(_)) => Ok(None),
        Some(interaction_update::Message::TurnEnded(usage)) => Ok(Some(ResponseEvent::Completed {
            response_id: response_id.to_string(),
            token_usage: map_token_usage(usage),
            end_turn: Some(true),
        })),
        None
        | Some(
            interaction_update::Message::ToolCallStarted(_)
            | interaction_update::Message::ToolCallCompleted(_)
            | interaction_update::Message::ThinkingDelta(_)
            | interaction_update::Message::ThinkingCompleted(_)
            | interaction_update::Message::UserMessageAppended(_)
            | interaction_update::Message::PartialToolCall(_)
            | interaction_update::Message::TokenDelta(_)
            | interaction_update::Message::Summary(_)
            | interaction_update::Message::SummaryStarted(_)
            | interaction_update::Message::SummaryCompleted(_)
            | interaction_update::Message::ShellOutputDelta(_)
            | interaction_update::Message::ToolCallDelta(_)
            | interaction_update::Message::StepStarted(_)
            | interaction_update::Message::StepCompleted(_)
            | interaction_update::Message::PromptSuggestion(_)
            | interaction_update::Message::PostRequestPrompt(_)
            | interaction_update::Message::ActiveBranchChange(_)
            | interaction_update::Message::FeedbackRequest(_)
            | interaction_update::Message::ResponseComparison(_),
        ) => Err(CursorMappingError::UnsupportedInteractionUpdate),
    }
}
