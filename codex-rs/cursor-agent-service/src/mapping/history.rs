use super::APPLY_PATCH_TOOL_NAME;
use super::CursorMappingError;
use super::tools::CursorToolSnapshot;
use super::tools::FrozenToolKind;
use super::values::map_historical_tool_output;
use super::values::parse_json_object;
use crate::proto::AgentConversationTurnStructure;
use crate::proto::AssistantMessage;
use crate::proto::ConversationStateStructure;
use crate::proto::ConversationStep;
use crate::proto::ConversationTurnStructure;
use crate::proto::McpArgs;
use crate::proto::McpToolCall;
use crate::proto::ToolCall;
use crate::proto::UserMessage;
use crate::proto::conversation_step;
use crate::proto::conversation_turn_structure;
use crate::proto::tool_call;
use codex_protocol::ToolName;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use prost::Message as _;
use serde_json::Map as JsonMap;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::collections::HashSet;

pub(super) fn map_conversation(
    input: &[ResponseItem],
    current_message_id: &str,
    synthesized_user_message: Option<&str>,
    snapshot: &CursorToolSnapshot,
) -> Result<(ConversationStateStructure, UserMessage), CursorMappingError> {
    let (history, current_user) = match input.last() {
        Some(last @ ResponseItem::Message { role, .. }) if role == "user" => {
            let history = &input[..input.len() - 1];
            (history, map_user_message(last, current_message_id)?)
        }
        _ => {
            let Some(text) = synthesized_user_message else {
                return Err(CursorMappingError::MissingCurrentUserMessage);
            };
            if current_message_id.is_empty() {
                return Err(CursorMappingError::EmptyCurrentMessageId);
            }
            (
                input,
                UserMessage {
                    text: text.to_string(),
                    message_id: current_message_id.to_string(),
                },
            )
        }
    };

    Ok((
        ConversationStateStructure {
            root_prompt_messages_json: Vec::new(),
            pending_tool_calls: Vec::new(),
            summary: None,
            plan: None,
            turns: map_history_turns(history, snapshot)?,
            mode: None,
        },
        current_user,
    ))
}

fn map_history_turns(
    history: &[ResponseItem],
    snapshot: &CursorToolSnapshot,
) -> Result<Vec<Vec<u8>>, CursorMappingError> {
    let mut turns = Vec::new();
    let mut current_turn: Option<HistoricalTurnBuilder> = None;
    let mut seen_call_ids = HashSet::new();

    for item in history {
        match item {
            ResponseItem::Message { role, .. } if role == "user" => {
                if let Some(turn) = current_turn.take() {
                    turns.push(turn.finish()?.encode_to_vec());
                }
                let fallback_id = format!("cooldex-history-{}", turns.len());
                current_turn = Some(HistoricalTurnBuilder::new(map_user_message(
                    item,
                    &fallback_id,
                )?));
            }
            ResponseItem::Message { role, content, .. } if role == "assistant" => {
                let turn = current_turn.as_mut().ok_or_else(|| {
                    CursorMappingError::HistoryItemBeforeUser("assistant message".to_string())
                })?;
                turn.push_assistant(message_text(content, "assistant")?);
            }
            ResponseItem::Message { role, .. } => {
                return Err(CursorMappingError::UnsupportedMessageRole(role.clone()));
            }
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                record_new_call_id(&mut seen_call_ids, call_id)?;
                let turn = current_turn.as_mut().ok_or_else(|| {
                    CursorMappingError::HistoryItemBeforeUser("function call".to_string())
                })?;
                let arguments = parse_json_object(arguments, call_id)?;
                let source_name = ToolName::new(namespace.clone(), name.clone());
                let (args, description, kind) =
                    snapshot.historical_args(&source_name, call_id, arguments)?;
                if kind != FrozenToolKind::Function {
                    return Err(CursorMappingError::HistoricalOutputKindMismatch(
                        call_id.clone(),
                    ));
                }
                turn.push_tool_call(call_id, args, description, kind)?;
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                input,
                ..
            } => {
                if name != APPLY_PATCH_TOOL_NAME || namespace.is_some() {
                    return Err(CursorMappingError::NonCanonicalCustomTool(call_id.clone()));
                }
                record_new_call_id(&mut seen_call_ids, call_id)?;
                let turn = current_turn.as_mut().ok_or_else(|| {
                    CursorMappingError::HistoryItemBeforeUser("custom tool call".to_string())
                })?;
                let mut arguments = JsonMap::new();
                arguments.insert("patch".to_string(), JsonValue::String(input.clone()));
                let source_name = ToolName::plain(APPLY_PATCH_TOOL_NAME);
                let (args, description, kind) =
                    snapshot.historical_args(&source_name, call_id, arguments)?;
                if kind != FrozenToolKind::ApplyPatch {
                    return Err(CursorMappingError::NonCanonicalCustomTool(call_id.clone()));
                }
                turn.push_tool_call(call_id, args, description, kind)?;
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => current_turn
                .as_mut()
                .ok_or_else(|| CursorMappingError::HistoricalOutputBeforeCall(call_id.clone()))?
                .push_tool_output(call_id, output, FrozenToolKind::Function)?,
            ResponseItem::CustomToolCallOutput {
                call_id,
                name,
                output,
                ..
            } => {
                if name.is_some() {
                    return Err(CursorMappingError::NonCanonicalCustomTool(call_id.clone()));
                }
                current_turn
                    .as_mut()
                    .ok_or_else(|| CursorMappingError::HistoricalOutputBeforeCall(call_id.clone()))?
                    .push_tool_output(call_id, output, FrozenToolKind::ApplyPatch)?;
            }
            unsupported => {
                return Err(CursorMappingError::UnsupportedHistoryItem(
                    response_item_kind(unsupported).to_string(),
                ));
            }
        }
    }

    if let Some(turn) = current_turn {
        turns.push(turn.finish()?.encode_to_vec());
    }
    Ok(turns)
}

struct HistoricalTurnBuilder {
    user_message: UserMessage,
    steps: Vec<Option<ConversationStep>>,
    calls: HashMap<String, HistoricalCall>,
}

struct HistoricalCall {
    slot: usize,
    args: McpArgs,
    description: Option<String>,
    kind: FrozenToolKind,
}

impl HistoricalTurnBuilder {
    fn new(user_message: UserMessage) -> Self {
        Self {
            user_message,
            steps: Vec::new(),
            calls: HashMap::new(),
        }
    }

    fn push_assistant(&mut self, text: String) {
        self.steps.push(Some(ConversationStep {
            message: Some(conversation_step::Message::AssistantMessage(
                AssistantMessage { text },
            )),
        }));
    }

    fn push_tool_call(
        &mut self,
        call_id: &str,
        args: McpArgs,
        description: Option<String>,
        kind: FrozenToolKind,
    ) -> Result<(), CursorMappingError> {
        if self.calls.contains_key(call_id) {
            return Err(CursorMappingError::DuplicateHistoricalCallId(
                call_id.to_string(),
            ));
        }
        let slot = self.steps.len();
        self.steps.push(None);
        self.calls.insert(
            call_id.to_string(),
            HistoricalCall {
                slot,
                args,
                description,
                kind,
            },
        );
        Ok(())
    }

    fn push_tool_output(
        &mut self,
        call_id: &str,
        output: &FunctionCallOutputPayload,
        output_kind: FrozenToolKind,
    ) -> Result<(), CursorMappingError> {
        let Some(call) = self.calls.get(call_id) else {
            return Err(CursorMappingError::HistoricalOutputBeforeCall(
                call_id.to_string(),
            ));
        };
        if call.kind != output_kind {
            return Err(CursorMappingError::HistoricalOutputKindMismatch(
                call_id.to_string(),
            ));
        }
        if self.steps[call.slot].is_some() {
            return Err(CursorMappingError::DuplicateHistoricalOutput(
                call_id.to_string(),
            ));
        }
        self.steps[call.slot] = Some(ConversationStep {
            message: Some(conversation_step::Message::ToolCall(ToolCall {
                tool_call_id: Some(call_id.to_string()),
                tool: Some(tool_call::Tool::McpToolCall(McpToolCall {
                    args: Some(call.args.clone()),
                    result: Some(map_historical_tool_output(output)?),
                    description: call.description.clone(),
                })),
            })),
        });
        Ok(())
    }

    fn finish(self) -> Result<ConversationTurnStructure, CursorMappingError> {
        let mut encoded_steps = Vec::with_capacity(self.steps.len());
        for (index, step) in self.steps.into_iter().enumerate() {
            let Some(step) = step else {
                let call_id = self
                    .calls
                    .iter()
                    .find_map(|(call_id, call)| (call.slot == index).then(|| call_id.clone()))
                    .unwrap_or_else(|| format!("slot-{index}"));
                return Err(CursorMappingError::IncompleteHistoricalCall(call_id));
            };
            encoded_steps.push(step.encode_to_vec());
        }
        Ok(ConversationTurnStructure {
            turn: Some(conversation_turn_structure::Turn::AgentConversationTurn(
                AgentConversationTurnStructure {
                    user_message: self.user_message.encode_to_vec(),
                    steps: encoded_steps,
                    request_id: None,
                    encrypted_model: None,
                    dynamic_tool_count: None,
                },
            )),
        })
    }
}

fn map_user_message(
    item: &ResponseItem,
    fallback_message_id: &str,
) -> Result<UserMessage, CursorMappingError> {
    let ResponseItem::Message {
        id, role, content, ..
    } = item
    else {
        return Err(CursorMappingError::UnsupportedHistoryItem(
            response_item_kind(item).to_string(),
        ));
    };
    if role != "user" {
        return Err(CursorMappingError::UnsupportedMessageRole(role.clone()));
    }
    let message_id = id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| fallback_message_id.to_string());
    if message_id.is_empty() {
        return Err(CursorMappingError::EmptyCurrentMessageId);
    }
    Ok(UserMessage {
        text: message_text(content, "user")?,
        message_id,
    })
}

fn message_text(content: &[ContentItem], role: &str) -> Result<String, CursorMappingError> {
    let mut parts = Vec::with_capacity(content.len());
    for item in content {
        match (role, item) {
            ("user", ContentItem::InputText { text })
            | ("assistant", ContentItem::OutputText { text }) => parts.push(text.as_str()),
            _ => {
                return Err(CursorMappingError::UnsupportedMessageContent(
                    role.to_string(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(CursorMappingError::UnsupportedMessageContent(
            role.to_string(),
        ));
    }
    Ok(parts.join("\n"))
}

fn record_new_call_id(
    seen_call_ids: &mut HashSet<String>,
    call_id: &str,
) -> Result<(), CursorMappingError> {
    if seen_call_ids.insert(call_id.to_string()) {
        Ok(())
    } else {
        Err(CursorMappingError::DuplicateHistoricalCallId(
            call_id.to_string(),
        ))
    }
}

fn response_item_kind(item: &ResponseItem) -> &'static str {
    match item {
        ResponseItem::AdditionalTools { .. } => "additional_tools",
        ResponseItem::Message { .. } => "message",
        ResponseItem::AgentMessage { .. } => "agent_message",
        ResponseItem::Reasoning { .. } => "reasoning",
        ResponseItem::LocalShellCall { .. } => "local_shell_call",
        ResponseItem::FunctionCall { .. } => "function_call",
        ResponseItem::ToolSearchCall { .. } => "tool_search_call",
        ResponseItem::FunctionCallOutput { .. } => "function_call_output",
        ResponseItem::CustomToolCall { .. } => "custom_tool_call",
        ResponseItem::CustomToolCallOutput { .. } => "custom_tool_call_output",
        ResponseItem::ToolSearchOutput { .. } => "tool_search_output",
        ResponseItem::WebSearchCall { .. } => "web_search_call",
        ResponseItem::ImageGenerationCall { .. } => "image_generation_call",
        ResponseItem::Compaction { .. } => "compaction",
        ResponseItem::CompactionTrigger {} => "compaction_trigger",
        ResponseItem::ContextCompaction { .. } => "context_compaction",
        ResponseItem::Other => "other",
    }
}
