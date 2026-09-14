// Merge-safety anchor: pre-compaction handoff synthesis is operation-local so generated text
// never changes live history, recovery state, rollout state, or normal continuation ownership.
use std::sync::Arc;

use crate::ResponseStream;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::context::PostCompactRecoveryContext;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::turn_context::TurnContext;
use codex_async_utils::OrCancelExt;
use codex_otel::SessionTelemetry;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::field;
use tracing::trace_span;
use tracing::warn;

pub(crate) const PRE_COMPACT_HANDOFF_INSTRUCTIONS: &str = "Prepare a concise, execution-ready prompt-to-self for continuing this work after context compaction. Target about 1,000 visible tokens. Preserve the objective, current execution state, accepted decisions and constraints, essential concrete anchors, uncertainty, and the next action only when work remains. Clearly distinguish established facts and accepted decisions from proposals or uncertainty. Do not invent pending user input, do not answer a historical user request again, and do not use tools.";

/// Frozen request inputs whose equality can reject installation from a stale source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreCompactHandoffSettings {
    pub(crate) model_info: Arc<ModelInfo>,
    reasoning_effort: Option<ReasoningEffortConfig>,
    reasoning_summary: ReasoningSummaryConfig,
    service_tier: Option<String>,
}

impl PreCompactHandoffSettings {
    pub(crate) fn from_step_context(step_context: &StepContext) -> Self {
        Self::from_resolved_step_settings(&step_context.settings)
    }

    pub(crate) fn from_resolved_step_settings(settings: &ResolvedStepSettings) -> Self {
        Self {
            model_info: Arc::clone(&settings.model_info),
            reasoning_effort: settings.reasoning_effort().cloned(),
            reasoning_summary: settings.reasoning_summary,
            service_tier: settings.service_tier.clone(),
        }
    }
}

/// The exact model-visible source captured after the compaction hook accepts an operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreCompactHandoffInputSnapshot {
    input: Vec<ResponseItem>,
    base_instructions: BaseInstructions,
}

impl PreCompactHandoffInputSnapshot {
    pub(crate) fn new(input: Vec<ResponseItem>, base_instructions: BaseInstructions) -> Self {
        Self {
            input,
            base_instructions,
        }
    }
}

/// Immutable source evidence retained with one operation-local preparation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreCompactHandoffSource {
    input: Vec<ResponseItem>,
    base_instructions: BaseInstructions,
    settings: PreCompactHandoffSettings,
}

impl PreCompactHandoffSource {
    pub(crate) fn from_snapshot(
        snapshot: PreCompactHandoffInputSnapshot,
        settings: PreCompactHandoffSettings,
    ) -> Self {
        Self {
            input: snapshot.input,
            base_instructions: snapshot.base_instructions,
            settings,
        }
    }

    pub(crate) async fn is_current_for(
        &self,
        sess: &Session,
        settings: &PreCompactHandoffSettings,
    ) -> CodexResult<bool> {
        if self.settings != *settings {
            return Ok(false);
        }
        let current = sess
            .snapshot_pre_compact_handoff_input(&settings.model_info)
            .await?;
        Ok(self.input == current.input && self.base_instructions == current.base_instructions)
    }

    fn synthesis_prompt(&self) -> Prompt {
        let mut input = self.input.clone();
        input.push(ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: PRE_COMPACT_HANDOFF_INSTRUCTIONS.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        });
        Prompt {
            input,
            tools: Arc::default(),
            parallel_tool_calls: false,
            base_instructions: self.base_instructions.clone(),
            output_schema: None,
            output_schema_strict: true,
            max_output_tokens: None,
            cyber_access_program: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreCompactHandoffFailure {
    RequestFailed,
    ContextWindowExceeded,
    UnexpectedOutput,
    EmptyOutput,
    StreamEnded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PreCompactHandoffOutcome {
    Available(String),
    Unavailable(PreCompactHandoffFailure),
    UnsupportedNoLiveThread,
}

/// A completed pre-compaction preparation, held only by the compaction operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedPreCompactHandoff {
    source: PreCompactHandoffSource,
    outcome: PreCompactHandoffOutcome,
    recovery_instructions: String,
}

impl PreparedPreCompactHandoff {
    pub(crate) fn source(&self) -> &PreCompactHandoffSource {
        &self.source
    }

    pub(crate) fn settings(&self) -> &PreCompactHandoffSettings {
        &self.source.settings
    }

    pub(crate) fn recovery_instructions(&self) -> &str {
        &self.recovery_instructions
    }

    pub(crate) fn handoff_text(&self) -> Option<&str> {
        match &self.outcome {
            PreCompactHandoffOutcome::Available(text) => Some(text),
            PreCompactHandoffOutcome::Unavailable(_)
            | PreCompactHandoffOutcome::UnsupportedNoLiveThread => None,
        }
    }
}

/// Prepares one hidden, tool-free handoff request without publishing generated text.
pub(crate) async fn prepare_pre_compact_handoff(
    sess: &Session,
    turn_context: &TurnContext,
    settings: PreCompactHandoffSettings,
    session_telemetry: &SessionTelemetry,
    cancellation_token: &CancellationToken,
) -> CodexResult<PreparedPreCompactHandoff> {
    if cancellation_token.is_cancelled() {
        return Err(CodexErr::TurnAborted);
    }
    let snapshot = sess
        .snapshot_pre_compact_handoff_input(&settings.model_info)
        .await?;
    let source = PreCompactHandoffSource::from_snapshot(snapshot, settings);
    let recovery_instructions = turn_context
        .config
        .post_compact_recovery_instructions
        .as_deref()
        .unwrap_or(PostCompactRecoveryContext::default_instructions())
        .to_string();
    if cancellation_token.is_cancelled() {
        return Err(CodexErr::TurnAborted);
    }
    if sess.live_thread().is_none() {
        return Ok(PreparedPreCompactHandoff {
            source,
            outcome: PreCompactHandoffOutcome::UnsupportedNoLiveThread,
            recovery_instructions,
        });
    }

    let prompt = source.synthesis_prompt();
    let responses_metadata = sess
        .responses_metadata(turn_context, CodexResponsesRequestKind::PreCompactHandoff)
        .await;
    let outcome = match synthesize_pre_compact_handoff(
        sess,
        &source,
        &prompt,
        session_telemetry,
        &responses_metadata,
        cancellation_token,
    )
    .await
    {
        Ok(Ok(text)) => PreCompactHandoffOutcome::Available(text),
        Ok(Err(failure)) => PreCompactHandoffOutcome::Unavailable(failure),
        Err(error) => boundary_outcome_from_error(error)?,
    };
    if let PreCompactHandoffOutcome::Unavailable(failure) = &outcome {
        warn!(
            handoff_failure = ?failure,
            "pre-compaction handoff synthesis unavailable; continuing with boundary-only recovery"
        );
    }
    Ok(PreparedPreCompactHandoff {
        source,
        outcome,
        recovery_instructions,
    })
}

async fn synthesize_pre_compact_handoff(
    sess: &Session,
    source: &PreCompactHandoffSource,
    prompt: &Prompt,
    session_telemetry: &SessionTelemetry,
    responses_metadata: &crate::CodexResponsesMetadata,
    cancellation_token: &CancellationToken,
) -> CodexResult<Result<String, PreCompactHandoffFailure>> {
    let mut client_session = sess.services.model_client.new_maintenance_session();
    let stream = match client_session
        .stream(
            prompt,
            &source.settings.model_info,
            session_telemetry,
            source.settings.reasoning_effort.clone(),
            source.settings.reasoning_summary,
            source.settings.service_tier.clone(),
            responses_metadata,
            &InferenceTraceContext::disabled(),
        )
        .or_cancel(cancellation_token)
        .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return boundary_result_from_error(error),
        Err(_) => return Err(CodexErr::TurnAborted),
    };
    let response_span = trace_span!(
        "pre_compact_handoff_synthesis",
        model = %source.settings.model_info.slug,
        otel.name = field::Empty,
        tool_name = field::Empty,
        from = field::Empty,
        gen_ai.usage.input_tokens = field::Empty,
        gen_ai.usage.cache_read.input_tokens = field::Empty,
        gen_ai.usage.cache_write.input_tokens = field::Empty,
        gen_ai.usage.output_tokens = field::Empty,
        codex.usage.reasoning_output_tokens = field::Empty,
        codex.usage.total_tokens = field::Empty,
    );
    collect_handoff_response(stream, cancellation_token, |event| {
        session_telemetry.record_responses(&response_span, event);
    })
    .await
}

fn boundary_result_from_error(
    error: CodexErr,
) -> CodexResult<Result<String, PreCompactHandoffFailure>> {
    boundary_outcome_from_error(error).map(|outcome| match outcome {
        PreCompactHandoffOutcome::Unavailable(failure) => Err(failure),
        PreCompactHandoffOutcome::Available(_)
        | PreCompactHandoffOutcome::UnsupportedNoLiveThread => {
            unreachable!("request failures can only produce boundary-only degradation")
        }
    })
}

fn boundary_outcome_from_error(error: CodexErr) -> CodexResult<PreCompactHandoffOutcome> {
    let failure = match error.details() {
        CodexErrorDetails::TurnAborted | CodexErrorDetails::Interrupted => return Err(error),
        CodexErrorDetails::ContextWindowExceeded => PreCompactHandoffFailure::ContextWindowExceeded,
        _ => PreCompactHandoffFailure::RequestFailed,
    };
    Ok(PreCompactHandoffOutcome::Unavailable(failure))
}

async fn collect_handoff_response<F>(
    mut stream: ResponseStream,
    cancellation_token: &CancellationToken,
    mut observe: F,
) -> CodexResult<Result<String, PreCompactHandoffFailure>>
where
    F: FnMut(&ResponseEvent),
{
    let mut collector = HandoffOutputCollector::default();
    loop {
        let event = match stream.next().or_cancel(cancellation_token).await {
            Ok(event) => event,
            Err(_) => return Err(CodexErr::TurnAborted),
        };
        let event = match event {
            Some(Ok(event)) => event,
            Some(Err(error)) => return boundary_result_from_error(error),
            None => return Ok(Err(PreCompactHandoffFailure::StreamEnded)),
        };
        observe(&event);
        match collector.observe(event) {
            Ok(Some(text)) => return Ok(Ok(text)),
            Ok(None) => {}
            Err(failure) => return Ok(Err(failure)),
        }
    }
}

#[derive(Default)]
struct HandoffOutputCollector {
    text: String,
}

impl HandoffOutputCollector {
    fn observe(
        &mut self,
        event: ResponseEvent,
    ) -> Result<Option<String>, PreCompactHandoffFailure> {
        match event {
            ResponseEvent::OutputItemAdded(item) => {
                if is_reasoning_item(&item) {
                    return Ok(None);
                }
                if !is_assistant_text_item(&item) {
                    return Err(PreCompactHandoffFailure::UnexpectedOutput);
                }
                Ok(None)
            }
            ResponseEvent::OutputItemDone(item) => {
                if is_reasoning_item(&item) {
                    return Ok(None);
                }
                if !is_assistant_text_item(&item) {
                    return Err(PreCompactHandoffFailure::UnexpectedOutput);
                }
                let ResponseItem::Message { content, .. } = item else {
                    unreachable!("assistant text items are messages")
                };
                for content_item in content {
                    let ContentItem::OutputText { text } = content_item else {
                        return Err(PreCompactHandoffFailure::UnexpectedOutput);
                    };
                    self.text.push_str(&text);
                }
                Ok(None)
            }
            ResponseEvent::ToolCallInputDelta { .. } => {
                Err(PreCompactHandoffFailure::UnexpectedOutput)
            }
            ResponseEvent::Completed { .. } => {
                if self.text.trim().is_empty() {
                    return Err(PreCompactHandoffFailure::EmptyOutput);
                }
                Ok(Some(std::mem::take(&mut self.text)))
            }
            _ => Ok(None),
        }
    }
}

fn is_reasoning_item(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Reasoning { .. })
}

fn is_assistant_text_item(item: &ResponseItem) -> bool {
    matches!(item,
        ResponseItem::Message { role, content, .. }
            if role == "assistant"
                && content.iter().all(|content| matches!(content, ContentItem::OutputText { .. }))
    )
}

#[cfg(test)]
#[path = "compact_handoff_tests.rs"]
mod tests;
