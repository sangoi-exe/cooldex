use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::client_common::SANITIZE_PROMPT;
use crate::codex::TurnContext;
use crate::codex::run_sampling_request;
use crate::error::CodexErr;
use crate::protocol::CompactedItem;
use crate::protocol::EventMsg;
use crate::protocol::RolloutItem;
use crate::protocol::TurnStartedEvent;
use crate::state::TaskKind;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::handlers::ManageContextHandler;
use crate::turn_diff_tracker::TurnDiffTracker;

use super::SessionTask;
use super::SessionTaskContext;

const SANITIZE_CONTEXT_WINDOW_EXCEEDED_MESSAGE: &str = "/sanitize could not continue because the context window is still full. Run /compact, then retry /sanitize.";
const SANITIZE_ERROR_MESSAGE_PREFIX: &str = "/sanitize failed:";
const SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE: &str =
    "/sanitize completed and applied context updates.";
const SANITIZE_COMPLETED_NO_CHANGES_MESSAGE: &str = "/sanitize completed with no context changes.";
const SANITIZE_ALLOWED_TOOL_NAME: &str = "manage_context";

#[derive(Clone, Debug, PartialEq, Eq)]
struct RetrieveSignature {
    chunk_fingerprints: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManageContextErrorSignature {
    stop_reason: String,
    message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ManageContextFollowUpEvent {
    Apply,
    Retrieve(RetrieveSignature),
    Error(ManageContextErrorSignature),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManageContextSeedMode {
    Retrieve,
    Apply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ManageContextSeedPairId {
    call_item_index: usize,
    output_item_index: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SanitizeMaterializationOutcome {
    history_cleanup_required: bool,
    semantic_context_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SanitizeHistoryMaterialization {
    replacement_history: Vec<ResponseItem>,
    history_cleanup_required: bool,
    semantic_context_changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SanitizeLoopDecision {
    Continue,
    ReachedFixedPoint,
    Stalled,
}

struct SanitizeStagnationTracker {
    fixed_point_k: usize,
    stalled_signature_threshold: usize,
    last_retrieve_signature: Option<RetrieveSignature>,
    stalled_signature_repeats: usize,
    fixed_point_window: VecDeque<RetrieveSignature>,
    idle_follow_up_requests: usize,
    consecutive_error_follow_up_requests: usize,
    last_manage_context_error: Option<ManageContextErrorSignature>,
    changed_history: bool,
}

impl SanitizeStagnationTracker {
    fn new(fixed_point_k: usize, stalled_signature_threshold: usize) -> Self {
        Self {
            fixed_point_k,
            stalled_signature_threshold,
            last_retrieve_signature: None,
            stalled_signature_repeats: 0,
            fixed_point_window: VecDeque::new(),
            idle_follow_up_requests: 0,
            consecutive_error_follow_up_requests: 0,
            last_manage_context_error: None,
            changed_history: false,
        }
    }

    fn record_follow_up_events(
        &mut self,
        events: &[ManageContextFollowUpEvent],
    ) -> SanitizeLoopDecision {
        if events.is_empty() {
            self.idle_follow_up_requests = self.idle_follow_up_requests.saturating_add(1);
            if self.idle_follow_up_requests >= self.stalled_signature_threshold {
                return SanitizeLoopDecision::Stalled;
            }
            return SanitizeLoopDecision::Continue;
        }

        self.idle_follow_up_requests = 0;
        let mut saw_progress_event = false;
        let mut latest_manage_context_error: Option<ManageContextErrorSignature> = None;
        for event in events {
            match event {
                ManageContextFollowUpEvent::Retrieve(signature) => {
                    saw_progress_event = true;
                    self.clear_manage_context_error_state();
                    let decision = self.record_retrieve_signature(signature.clone());
                    if !matches!(decision, SanitizeLoopDecision::Continue) {
                        return decision;
                    }
                }
                ManageContextFollowUpEvent::Apply => {
                    saw_progress_event = true;
                    self.clear_manage_context_error_state();
                }
                ManageContextFollowUpEvent::Error(error_signature) => {
                    latest_manage_context_error = Some(error_signature.clone());
                }
            }
        }

        if !saw_progress_event && let Some(error_signature) = latest_manage_context_error {
            self.record_manage_context_error(error_signature);
            if self.consecutive_error_follow_up_requests >= self.stalled_signature_threshold {
                return SanitizeLoopDecision::Stalled;
            }
        }

        SanitizeLoopDecision::Continue
    }

    fn record_retrieve_signature(&mut self, signature: RetrieveSignature) -> SanitizeLoopDecision {
        let repeated_signature = self.last_retrieve_signature.as_ref() == Some(&signature);
        if repeated_signature {
            self.stalled_signature_repeats = self.stalled_signature_repeats.saturating_add(1);
        } else {
            self.stalled_signature_repeats = 0;
        }

        self.last_retrieve_signature = Some(signature.clone());
        self.fixed_point_window.push_back(signature.clone());
        while self.fixed_point_window.len() > self.fixed_point_k {
            self.fixed_point_window.pop_front();
        }

        let fixed_point_reached = signature.chunk_fingerprints.is_empty()
            && self.fixed_point_window.len() == self.fixed_point_k
            && self
                .fixed_point_window
                .iter()
                .all(|seen| seen == &signature);
        if fixed_point_reached {
            return SanitizeLoopDecision::ReachedFixedPoint;
        }

        if !signature.chunk_fingerprints.is_empty()
            && self.stalled_signature_repeats >= self.stalled_signature_threshold
        {
            return SanitizeLoopDecision::Stalled;
        }

        SanitizeLoopDecision::Continue
    }

    fn stalled_loop_message(&self) -> String {
        let history_impact = self.stalled_history_impact_suffix();
        if let Some(error_signature) = &self.last_manage_context_error {
            let mut message = format!(
                "/sanitize stopped because manage_context returned stop_reason='{}' for {} consecutive follow-up cycles (manage_context_policy.stalled_signature_threshold={}).{} Run /compact, then retry /sanitize.",
                error_signature.stop_reason,
                self.consecutive_error_follow_up_requests,
                self.stalled_signature_threshold,
                history_impact,
            );
            if let Some(raw_message) = error_signature.message.as_deref() {
                let trimmed_message = raw_message.trim();
                if !trimmed_message.is_empty() {
                    let compact_message = summarize_status_message(trimmed_message, 220);
                    message.push_str(" Last manage_context error: ");
                    message.push_str(&compact_message);
                    message.push('.');
                }
            }
            return message;
        }
        if self.idle_follow_up_requests >= self.stalled_signature_threshold {
            return format!(
                "/sanitize stopped because the model requested follow-up without producing a parseable manage_context output for {} cycles (manage_context_policy.stalled_signature_threshold={}).{} Run /compact, then retry /sanitize.",
                self.idle_follow_up_requests, self.stalled_signature_threshold, history_impact
            );
        }
        format!(
            "/sanitize stopped because manage_context retrieve signatures are stalled for {} repeats (manage_context_policy.stalled_signature_threshold={}).{} Run /compact, then retry /sanitize.",
            self.stalled_signature_repeats, self.stalled_signature_threshold, history_impact
        )
    }

    fn set_changed_history(&mut self, changed_history: bool) {
        self.changed_history = changed_history;
    }

    fn clear_manage_context_error_state(&mut self) {
        self.consecutive_error_follow_up_requests = 0;
        self.last_manage_context_error = None;
    }

    fn record_manage_context_error(&mut self, error_signature: ManageContextErrorSignature) {
        self.last_manage_context_error = Some(error_signature);
        self.consecutive_error_follow_up_requests =
            self.consecutive_error_follow_up_requests.saturating_add(1);
    }

    fn stalled_history_impact_suffix(&self) -> &'static str {
        if self.changed_history {
            " Context updates were applied before stopping."
        } else {
            " No context updates were applied."
        }
    }
}

fn summarize_status_message(message: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    if max_chars <= 3 {
        return "...".chars().take(max_chars).collect();
    }
    let truncated = normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    let truncated = truncated.trim_end();
    format!("{truncated}...")
}

fn build_sanitize_prompt(policy: &crate::config::ManageContextPolicy) -> String {
    // Merge-safety anchor: this runtime policy block must stay aligned with
    // manage_context contract validation and core/sanitize_prompt.md text.
    format!(
        "{SANITIZE_PROMPT}\n\nRuntime manage_context policy (authoritative):\n- policy_id: {}\n- fixed_point_k: {}\n- stalled_signature_threshold: {}\n- max_chunks_per_apply: {}",
        policy.quality_rubric_id,
        policy.fixed_point_k,
        policy.stalled_signature_threshold,
        policy.max_chunks_per_apply,
    )
}

fn sanitize_allowed_tool_names() -> HashSet<String> {
    HashSet::from([SANITIZE_ALLOWED_TOOL_NAME.to_string()])
}

async fn materialize_sanitize_history_if_changed(
    sess: &crate::codex::Session,
    ctx: &TurnContext,
    history_before_sanitize: &[ResponseItem],
    sanitize_generated_non_tool_items: &[ResponseItem],
) -> Result<SanitizeMaterializationOutcome, CodexErr> {
    let (materialization, checkpoint) = {
        let mut state = sess.state.lock().await;
        let checkpoint = state.manage_context_checkpoint();
        (
            sanitize_replacement_history_if_changed(
                &mut state,
                history_before_sanitize,
                sanitize_generated_non_tool_items,
            ),
            checkpoint,
        )
    };
    let outcome = materialization.as_ref().map_or(
        SanitizeMaterializationOutcome::default(),
        |materialization| SanitizeMaterializationOutcome {
            history_cleanup_required: materialization.history_cleanup_required,
            semantic_context_changed: materialization.semantic_context_changed,
        },
    );
    if let Some(materialization) = materialization {
        // Merge-safety anchor: persisting replacement_history here defines the
        // sanitized boundary consumed by recall and resume replay.
        let compacted_item = RolloutItem::Compacted(CompactedItem {
            message: String::new(),
            replacement_history: Some(materialization.replacement_history),
        });
        let recorder = {
            let guard = sess.services.rollout.lock().await;
            guard.clone()
        };
        if let Some(recorder) = recorder
            && let Err(error) = recorder.record_items(&[compacted_item]).await
        {
            let mut state = sess.state.lock().await;
            state.restore_manage_context_checkpoint(checkpoint);
            return Err(CodexErr::Fatal(format!(
                "failed to persist compacted replacement_history: {error}"
            )));
        }
    }
    sess.recompute_token_usage(ctx).await;
    Ok(outcome)
}

async fn report_sanitize_materialization_error(
    sess: &crate::codex::Session,
    ctx: &TurnContext,
    error: CodexErr,
) -> Option<String> {
    sess.send_event(ctx, EventMsg::Error(error.to_error_event(None)))
        .await;
    Some(format!(
        "{SANITIZE_ERROR_MESSAGE_PREFIX} {error}. Fix the error and retry /sanitize."
    ))
}

#[derive(Clone, Copy)]
pub(crate) struct SanitizeTask;

impl SanitizeTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionTask for SanitizeTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Sanitize
    }

    fn span_name(&self) -> &'static str {
        "session_task.sanitize"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let sess = session.clone_session();
        let manage_context_policy = ctx.config.manage_context_policy.clone();

        let started = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: ctx.sub_id.clone(),
            model_context_window: ctx.model_context_window(),
            collaboration_mode_kind: ctx.collaboration_mode.mode,
        });
        sess.send_event(ctx.as_ref(), started).await;

        let sanitize_prompt = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: build_sanitize_prompt(&manage_context_policy),
            }],
            end_turn: None,
            phase: None,
        };

        let turn_diff_tracker: SharedTurnDiffTracker =
            Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let mut client_session = sess.services.model_client.new_session();
        let explicitly_enabled_connectors = HashSet::new();
        let allowed_tool_names = sanitize_allowed_tool_names();
        let history_before_sanitize = {
            let state = sess.state.lock().await;
            state.history_snapshot_lenient()
        };
        let mut stagnation_tracker = SanitizeStagnationTracker::new(
            manage_context_policy.fixed_point_k,
            manage_context_policy.stalled_signature_threshold,
        );
        let mut sanitize_generated_non_tool_items: Vec<ResponseItem> = Vec::new();
        let mut server_model_warning_emitted_for_turn = false;

        loop {
            if cancellation_token.is_cancelled() {
                return None;
            }

            let history_before_request = {
                let state = sess.state.lock().await;
                state.history_snapshot_lenient()
            };
            let manage_context_items = collect_manage_context_seed_items(&history_before_request);

            let mut input = Vec::with_capacity(1 + manage_context_items.len());
            input.push(sanitize_prompt.clone());
            input.extend(manage_context_items);

            let output = match run_sampling_request(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                Arc::clone(&turn_diff_tracker),
                &mut client_session,
                None,
                input,
                &explicitly_enabled_connectors,
                None,
                Some(&allowed_tool_names),
                &mut server_model_warning_emitted_for_turn,
                cancellation_token.child_token(),
            )
            .await
            {
                Ok(output) => output,
                Err(CodexErr::TurnAborted | CodexErr::Interrupted) => return None,
                Err(error @ CodexErr::ContextWindowExceeded) => {
                    sess.set_total_tokens_full(ctx.as_ref()).await;
                    sess.send_event(ctx.as_ref(), EventMsg::Error(error.to_error_event(None)))
                        .await;
                    return Some(SANITIZE_CONTEXT_WINDOW_EXCEEDED_MESSAGE.to_string());
                }
                Err(error) => {
                    sess.send_event(ctx.as_ref(), EventMsg::Error(error.to_error_event(None)))
                        .await;
                    return Some(format!(
                        "{SANITIZE_ERROR_MESSAGE_PREFIX} {error}. Fix the error and retry /sanitize."
                    ));
                }
            };
            let needs_follow_up = output.needs_follow_up;
            let last_agent_message = output.last_agent_message;
            sanitize_generated_non_tool_items.extend(output.non_tool_response_items);

            if !needs_follow_up {
                let materialization = match materialize_sanitize_history_if_changed(
                    sess.as_ref(),
                    ctx.as_ref(),
                    &history_before_sanitize,
                    &sanitize_generated_non_tool_items,
                )
                .await
                {
                    Ok(materialization) => materialization,
                    Err(error) => {
                        return report_sanitize_materialization_error(
                            sess.as_ref(),
                            ctx.as_ref(),
                            error,
                        )
                        .await;
                    }
                };
                return Some(sanitize_completion_message(
                    last_agent_message,
                    materialization.semantic_context_changed,
                ));
            }

            let follow_up_events = {
                let state = sess.state.lock().await;
                let items_after_request = state.history_snapshot_lenient();
                manage_context_follow_up_events_since(&history_before_request, &items_after_request)
            };

            match stagnation_tracker.record_follow_up_events(&follow_up_events) {
                SanitizeLoopDecision::Continue => {}
                SanitizeLoopDecision::ReachedFixedPoint => {
                    let materialization = match materialize_sanitize_history_if_changed(
                        sess.as_ref(),
                        ctx.as_ref(),
                        &history_before_sanitize,
                        &sanitize_generated_non_tool_items,
                    )
                    .await
                    {
                        Ok(materialization) => materialization,
                        Err(error) => {
                            return report_sanitize_materialization_error(
                                sess.as_ref(),
                                ctx.as_ref(),
                                error,
                            )
                            .await;
                        }
                    };
                    return Some(sanitize_completion_message(
                        last_agent_message,
                        materialization.semantic_context_changed,
                    ));
                }
                SanitizeLoopDecision::Stalled => {
                    let materialization = match materialize_sanitize_history_if_changed(
                        sess.as_ref(),
                        ctx.as_ref(),
                        &history_before_sanitize,
                        &sanitize_generated_non_tool_items,
                    )
                    .await
                    {
                        Ok(materialization) => materialization,
                        Err(error) => {
                            return report_sanitize_materialization_error(
                                sess.as_ref(),
                                ctx.as_ref(),
                                error,
                            )
                            .await;
                        }
                    };
                    stagnation_tracker
                        .set_changed_history(materialization.semantic_context_changed);
                    return Some(stagnation_tracker.stalled_loop_message());
                }
            }
        }
    }
}

fn sanitize_completion_message(
    last_agent_message: Option<String>,
    semantic_context_changed: bool,
) -> String {
    if !semantic_context_changed {
        return SANITIZE_COMPLETED_NO_CHANGES_MESSAGE.to_string();
    }

    if let Some(message) = last_agent_message
        && !message.trim().is_empty()
    {
        return message;
    }

    SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE.to_string()
}

fn strip_sanitize_generated_non_tool_items(
    prompt_snapshot: &mut Vec<ResponseItem>,
    sanitize_generated_non_tool_items: &[ResponseItem],
) {
    for item in sanitize_generated_non_tool_items.iter().rev() {
        if let Some(index) = prompt_snapshot
            .iter()
            .rposition(|candidate| candidate == item)
        {
            prompt_snapshot.remove(index);
        }
    }
}

fn sanitize_replacement_history_if_changed(
    state: &mut crate::state::SessionState,
    history_before_sanitize: &[ResponseItem],
    sanitize_generated_non_tool_items: &[ResponseItem],
) -> Option<SanitizeHistoryMaterialization> {
    let current_history = state.history_snapshot_lenient();
    let mut stripped_prompt_snapshot = state.prompt_snapshot_lenient();
    ManageContextHandler::strip_completed_manage_context_pairs_from_prompt_snapshot(
        &current_history,
        &mut stripped_prompt_snapshot,
    );
    strip_sanitize_generated_non_tool_items(
        &mut stripped_prompt_snapshot,
        sanitize_generated_non_tool_items,
    );
    let history_cleanup_required = stripped_prompt_snapshot != current_history;
    let semantic_context_changed = stripped_prompt_snapshot != history_before_sanitize;

    if !history_cleanup_required && !semantic_context_changed {
        return None;
    }

    if history_cleanup_required {
        let reference_context_item = state.reference_context_item();
        state.replace_history(stripped_prompt_snapshot.clone(), reference_context_item);
    }

    Some(SanitizeHistoryMaterialization {
        replacement_history: stripped_prompt_snapshot,
        history_cleanup_required,
        semantic_context_changed,
    })
}

fn parse_manage_context_output_item(item: &ResponseItem) -> Option<Value> {
    let text = match item {
        ResponseItem::FunctionCallOutput { output, .. } => output.body.to_text()?,
        ResponseItem::CustomToolCallOutput { output, .. } => output.body.to_text()?,
        _ => return None,
    };

    serde_json::from_str::<Value>(&text).ok()
}

#[cfg(test)]
fn manage_context_follow_up_events_from_items(
    items: &[ResponseItem],
) -> Vec<ManageContextFollowUpEvent> {
    items
        .iter()
        .filter_map(parse_manage_context_output_item)
        .filter_map(|output_value| parse_manage_context_follow_up_event(&output_value))
        .collect()
}

#[derive(Default)]
struct ManageContextCallOwnership {
    manage_context_function_call_ids: HashSet<String>,
    manage_context_custom_call_ids: HashSet<String>,
    non_manage_function_like_call_ids: HashSet<String>,
    non_manage_custom_call_ids: HashSet<String>,
}

impl ManageContextCallOwnership {
    fn from_items(items: &[ResponseItem]) -> Self {
        let mut call_ownership = Self::default();
        for item in items {
            match item {
                ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                    call_ownership
                        .manage_context_function_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                    call_ownership
                        .manage_context_custom_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::FunctionCall { call_id, .. } => {
                    call_ownership
                        .non_manage_function_like_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => {
                    call_ownership
                        .non_manage_function_like_call_ids
                        .insert(call_id.clone());
                }
                ResponseItem::CustomToolCall { call_id, .. } => {
                    call_ownership
                        .non_manage_custom_call_ids
                        .insert(call_id.clone());
                }
                _ => {}
            }
        }
        call_ownership
    }

    fn has_manage_context_calls(&self) -> bool {
        !self.manage_context_function_call_ids.is_empty()
            || !self.manage_context_custom_call_ids.is_empty()
    }

    fn is_manage_context_output(&self, item: &ResponseItem) -> bool {
        match item {
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                self.manage_context_function_call_ids.contains(call_id)
                    && !self.non_manage_function_like_call_ids.contains(call_id)
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                self.manage_context_custom_call_ids.contains(call_id)
                    && !self.non_manage_custom_call_ids.contains(call_id)
            }
            _ => false,
        }
    }
}

fn manage_context_output_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        _ => None,
    }
}

fn manage_context_output_signature(item: &ResponseItem) -> Option<(String, String, Value)> {
    let call_id = manage_context_output_call_id(item)?.to_string();
    let output_value = parse_manage_context_output_item(item)?;
    let output_signature = serde_json::to_string(&output_value).ok()?;
    Some((call_id, output_signature, output_value))
}

fn manage_context_follow_up_events_since(
    before: &[ResponseItem],
    after: &[ResponseItem],
) -> Vec<ManageContextFollowUpEvent> {
    let before_call_ownership = ManageContextCallOwnership::from_items(before);
    let after_call_ownership = ManageContextCallOwnership::from_items(after);
    if !after_call_ownership.has_manage_context_calls() {
        return Vec::new();
    }

    let mut seen_output_signatures: HashMap<(String, String), usize> = before
        .iter()
        .filter_map(|item| {
            if !before_call_ownership.is_manage_context_output(item) {
                return None;
            }
            let (call_id, output_signature, _) = manage_context_output_signature(item)?;
            Some((call_id, output_signature))
        })
        .fold(HashMap::new(), |mut seen, key| {
            *seen.entry(key).or_default() += 1;
            seen
        });

    after
        .iter()
        .filter_map(|item| {
            if !after_call_ownership.is_manage_context_output(item) {
                return None;
            }
            let (call_id, output_signature, output_value) = manage_context_output_signature(item)?;
            let signature_key = (call_id, output_signature);
            if let Some(remaining) = seen_output_signatures.get_mut(&signature_key)
                && *remaining > 0
            {
                *remaining -= 1;
                return None;
            }
            parse_manage_context_follow_up_event(&output_value)
        })
        .collect()
}

fn retrieve_signature_fingerprint(chunk: &Value) -> Option<String> {
    let approx_bytes = chunk
        .get("approx_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_default();

    if let Some(source_id) = chunk.get("source_id").and_then(Value::as_str) {
        return Some(format!("source:{source_id}|{approx_bytes}"));
    }
    if let Some(call_id) = chunk.get("call_id").and_then(Value::as_str) {
        return Some(format!("call:{call_id}|{approx_bytes}"));
    }

    let category = chunk
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let index = chunk
        .get("index")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    Some(format!("index:{index}:{category}|{approx_bytes}"))
}

fn parse_manage_context_follow_up_event(output: &Value) -> Option<ManageContextFollowUpEvent> {
    if let Some(mode) = output.get("mode").and_then(Value::as_str) {
        return match mode {
            "apply" => Some(ManageContextFollowUpEvent::Apply),
            "retrieve" => {
                let chunk_fingerprints = output
                    .get("chunk_manifest")
                    .and_then(Value::as_array)
                    .map(|chunks| {
                        chunks
                            .iter()
                            .filter_map(retrieve_signature_fingerprint)
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default();

                Some(ManageContextFollowUpEvent::Retrieve(RetrieveSignature {
                    chunk_fingerprints,
                }))
            }
            _ => None,
        };
    }

    let stop_reason = output.get("stop_reason").and_then(Value::as_str)?;
    let message = output
        .get("message")
        .and_then(Value::as_str)
        .map(std::string::ToString::to_string);
    Some(ManageContextFollowUpEvent::Error(
        ManageContextErrorSignature {
            stop_reason: stop_reason.to_string(),
            message,
        },
    ))
}

fn manage_context_seed_mode(output: &Value) -> Option<ManageContextSeedMode> {
    match parse_manage_context_follow_up_event(output)? {
        ManageContextFollowUpEvent::Retrieve(_) => Some(ManageContextSeedMode::Retrieve),
        ManageContextFollowUpEvent::Apply => Some(ManageContextSeedMode::Apply),
        ManageContextFollowUpEvent::Error(_) => None,
    }
}

fn select_manage_context_seed_pairs(items: &[ResponseItem]) -> Vec<ManageContextSeedPairId> {
    let call_ownership = ManageContextCallOwnership::from_items(items);
    let mut pending_function_calls: HashMap<String, VecDeque<usize>> = HashMap::new();
    let mut pending_custom_calls: HashMap<String, VecDeque<usize>> = HashMap::new();
    let mut latest_retrieve_pair: Option<ManageContextSeedPairId> = None;
    let mut latest_apply_pair: Option<ManageContextSeedPairId> = None;

    for (item_index, item) in items.iter().enumerate() {
        match item {
            ResponseItem::FunctionCall { name, call_id, .. }
                if name == SANITIZE_ALLOWED_TOOL_NAME =>
            {
                pending_function_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push_back(item_index);
            }
            ResponseItem::CustomToolCall { name, call_id, .. }
                if name == SANITIZE_ALLOWED_TOOL_NAME =>
            {
                pending_custom_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push_back(item_index);
            }
            ResponseItem::FunctionCallOutput { call_id, .. }
                if call_ownership.is_manage_context_output(item) =>
            {
                let Some(call_item_index) = pending_function_calls
                    .get_mut(call_id.as_str())
                    .and_then(VecDeque::pop_front)
                else {
                    continue;
                };
                let Some(output_value) = parse_manage_context_output_item(item) else {
                    continue;
                };
                let Some(mode) = manage_context_seed_mode(&output_value) else {
                    continue;
                };
                let pair_id = ManageContextSeedPairId {
                    call_item_index,
                    output_item_index: item_index,
                };
                match mode {
                    ManageContextSeedMode::Retrieve => latest_retrieve_pair = Some(pair_id),
                    ManageContextSeedMode::Apply => latest_apply_pair = Some(pair_id),
                }
            }
            ResponseItem::CustomToolCallOutput { call_id, .. }
                if call_ownership.is_manage_context_output(item) =>
            {
                let Some(call_item_index) = pending_custom_calls
                    .get_mut(call_id.as_str())
                    .and_then(VecDeque::pop_front)
                else {
                    continue;
                };
                let Some(output_value) = parse_manage_context_output_item(item) else {
                    continue;
                };
                let Some(mode) = manage_context_seed_mode(&output_value) else {
                    continue;
                };
                let pair_id = ManageContextSeedPairId {
                    call_item_index,
                    output_item_index: item_index,
                };
                match mode {
                    ManageContextSeedMode::Retrieve => latest_retrieve_pair = Some(pair_id),
                    ManageContextSeedMode::Apply => latest_apply_pair = Some(pair_id),
                }
            }
            _ => {}
        }
    }

    let mut selected_pairs = Vec::new();
    if let Some(pair_id) = latest_retrieve_pair {
        selected_pairs.push(pair_id);
    }
    if let Some(pair_id) = latest_apply_pair {
        selected_pairs.push(pair_id);
    }
    selected_pairs
}

fn collect_manage_context_seed_items(items: &[ResponseItem]) -> Vec<ResponseItem> {
    let selected_pairs = select_manage_context_seed_pairs(items);
    if selected_pairs.is_empty() {
        return Vec::new();
    }
    let selected_indices: HashSet<usize> = selected_pairs
        .into_iter()
        .flat_map(|pair_id| [pair_id.call_item_index, pair_id.output_item_index])
        .collect();
    items
        .iter()
        .enumerate()
        .filter(|(item_index, _)| selected_indices.contains(item_index))
        .map(|(_, item)| item.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_configuration_for_tests;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::LocalShellAction;
    use codex_protocol::models::LocalShellExecAction;
    use codex_protocol::models::LocalShellStatus;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn input_text_message(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn manage_context_call(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "manage_context".to_string(),
            arguments: "{}".to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn function_call_output(call_id: &str, content: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                body: codex_protocol::models::FunctionCallOutputBody::Text(content.to_string()),
                success: Some(true),
            },
        }
    }

    fn manage_context_call_with_arguments(call_id: &str, arguments: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "manage_context".to_string(),
            arguments: arguments.to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn non_manage_call(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "other_tool".to_string(),
            arguments: "{}".to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn local_shell_call(call_id: &str) -> ResponseItem {
        ResponseItem::LocalShellCall {
            id: None,
            call_id: Some(call_id.to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["echo".to_string(), "ok".to_string()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
        }
    }

    fn custom_manage_context_call(call_id: &str, input: &str) -> ResponseItem {
        ResponseItem::CustomToolCall {
            id: None,
            status: Some("completed".to_string()),
            call_id: call_id.to_string(),
            name: "manage_context".to_string(),
            input: input.to_string(),
        }
    }

    fn custom_manage_context_output(call_id: &str, content: &str) -> ResponseItem {
        ResponseItem::CustomToolCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                body: codex_protocol::models::FunctionCallOutputBody::Text(content.to_string()),
                success: Some(true),
            },
        }
    }

    #[test]
    fn build_sanitize_prompt_includes_runtime_policy() {
        let policy = crate::config::ManageContextPolicy {
            fixed_point_k: 3,
            stalled_signature_threshold: 4,
            max_chunks_per_apply: 9,
            quality_rubric_id: "sanitize_strict".to_string(),
        };

        let prompt = build_sanitize_prompt(&policy);
        assert!(prompt.contains("policy_id: sanitize_strict"));
        assert!(prompt.contains("fixed_point_k: 3"));
        assert!(prompt.contains("stalled_signature_threshold: 4"));
        assert!(prompt.contains("max_chunks_per_apply: 9"));
    }

    #[test]
    fn sanitize_allowed_tool_names_only_includes_manage_context() {
        let allowed_tool_names = sanitize_allowed_tool_names();
        assert_eq!(allowed_tool_names.len(), 1);
        assert!(allowed_tool_names.contains("manage_context"));
    }

    #[test]
    fn collect_manage_context_seed_items_preserves_apply_call_contract_fields_without_compaction() {
        let items = vec![
            manage_context_call_with_arguments(
                "call-1",
                &json!({
                    "mode": "apply",
                    "policy_id": "sanitize_strict",
                    "plan_id": "plan-1",
                    "state_hash": "state-1",
                    "chunk_summaries": [{
                        "chunk_id": "chunk_001",
                        "tool_context": "tool summary",
                        "reasoning_context": "reasoning summary"
                    }]
                })
                .to_string(),
            ),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let output = collect_manage_context_seed_items(&items);
        assert_eq!(output.len(), 2);

        let Some(ResponseItem::FunctionCall { arguments, .. }) = output.first() else {
            panic!("expected function_call");
        };
        let arguments: Value = serde_json::from_str(arguments).expect("valid JSON arguments");
        assert_eq!(arguments.get("mode").and_then(Value::as_str), Some("apply"));
        assert_eq!(
            arguments.get("policy_id").and_then(Value::as_str),
            Some("sanitize_strict")
        );
        assert_eq!(
            arguments.get("plan_id").and_then(Value::as_str),
            Some("plan-1")
        );
        assert_eq!(
            arguments.get("state_hash").and_then(Value::as_str),
            Some("state-1")
        );
        assert_eq!(
            arguments.pointer("/chunk_summaries/0/chunk_id"),
            Some(&json!("chunk_001"))
        );
        assert_eq!(
            arguments.pointer("/chunk_summaries/0/tool_context"),
            Some(&json!("tool summary"))
        );
        assert_eq!(
            arguments.pointer("/chunk_summaries/0/reasoning_context"),
            Some(&json!("reasoning summary"))
        );
    }

    #[test]
    fn collect_manage_context_seed_items_preserves_large_retrieve_output_for_v2_contract() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "policy_id": "sanitize_strict",
                    "plan_id": "plan-1",
                    "state_hash": "state-1",
                    "convergence_policy": {
                        "fixed_point_k": 2,
                        "stalled_signature_threshold": 2,
                        "max_chunks_per_apply": 8,
                        "quality_rubric_id": "sanitize_strict"
                    },
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "index": 17,
                        "category": "tool_output",
                        "call_id": "call-heavy",
                        "approx_bytes": 50000,
                        "preview": "x".repeat(30_000)
                    }],
                    "top_offenders": [{
                        "id": "r42",
                        "index": 17,
                        "category": "tool_output",
                        "approx_bytes": 50000,
                        "call_id": "call-heavy",
                        "tool_name": "exec_command",
                        "preview": "x".repeat(30_000)
                    }]
                })
                .to_string(),
            ),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(output.len(), 2);
        let Some(ResponseItem::FunctionCallOutput { output, .. }) = output.get(1) else {
            panic!("expected function_call_output");
        };
        let retrieve_output = output
            .body
            .to_text()
            .expect("text output for manage_context");
        let retrieve_output: Value = serde_json::from_str(&retrieve_output)
            .expect("retrieve output in seed must be valid JSON");
        assert_eq!(
            retrieve_output.get("plan_id").and_then(Value::as_str),
            Some("plan-1")
        );
        assert_eq!(
            retrieve_output.get("state_hash").and_then(Value::as_str),
            Some("state-1")
        );
        assert_eq!(
            retrieve_output.pointer("/chunk_manifest/0/preview"),
            Some(&json!("x".repeat(30_000)))
        );
        assert_eq!(
            retrieve_output.pointer("/top_offenders/0/preview"),
            Some(&json!("x".repeat(30_000)))
        );
    }

    #[test]
    fn collect_manage_context_seed_items_preserves_apply_output_for_v2_contract() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached",
                    "new_state_hash": "state-2",
                    "progress_report": {
                        "requested_chunks": 1,
                        "applied_chunks": 1,
                        "manifest_chunk_count_before": 8,
                        "remaining_manifest_chunks": 7,
                        "max_chunks_per_apply": 8
                    },
                    "applied_events": [{
                        "chunk_id": "chunk_001",
                        "tool_context": "tool context ".repeat(1_000),
                        "reasoning_context": "reasoning context ".repeat(1_000)
                    }]
                })
                .to_string(),
            ),
        ];

        let output = collect_manage_context_seed_items(&items);
        assert_eq!(output.len(), 2);

        let Some(ResponseItem::FunctionCallOutput { output, .. }) = output.get(1) else {
            panic!("expected function_call_output");
        };
        let apply_output = output
            .body
            .to_text()
            .expect("text output for manage_context");
        let apply_output: Value =
            serde_json::from_str(&apply_output).expect("apply output in seed must be valid JSON");
        assert_eq!(
            apply_output.get("new_state_hash").and_then(Value::as_str),
            Some("state-2")
        );
        assert_eq!(
            apply_output.pointer("/applied_events/0/chunk_id"),
            Some(&json!("chunk_001"))
        );
        assert_eq!(
            apply_output
                .pointer("/applied_events/0/tool_context")
                .and_then(Value::as_str)
                .map(str::len),
            Some("tool context ".len() * 1_000)
        );
        assert_eq!(
            apply_output
                .pointer("/applied_events/0/reasoning_context")
                .and_then(Value::as_str)
                .map(str::len),
            Some("reasoning context ".len() * 1_000)
        );
    }

    #[test]
    fn collect_manage_context_seed_items_skips_incomplete_call_output_pairs() {
        let items = vec![manage_context_call("call-1")];

        let output = collect_manage_context_seed_items(&items);

        assert!(output.is_empty());
    }

    #[test]
    fn collect_manage_context_seed_items_keeps_latest_retrieve_and_apply_pairs_in_original_order() {
        let items = vec![
            manage_context_call("retrieve-1"),
            function_call_output(
                "retrieve-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r1",
                        "approx_bytes": 1000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("apply-1"),
            function_call_output(
                "apply-1",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
            manage_context_call("retrieve-2"),
            function_call_output(
                "retrieve-2",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_002",
                        "source_id": "r2",
                        "approx_bytes": 2000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("apply-2"),
            function_call_output(
                "apply-2",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
            manage_context_call("retrieve-3"),
            function_call_output(
                "retrieve-3",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_003",
                        "source_id": "r3",
                        "approx_bytes": 3000
                    }]
                })
                .to_string(),
            ),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(
            output,
            vec![
                manage_context_call("apply-2"),
                function_call_output(
                    "apply-2",
                    &json!({
                        "mode": "apply",
                        "stop_reason": "target_reached"
                    })
                    .to_string(),
                ),
                manage_context_call("retrieve-3"),
                function_call_output(
                    "retrieve-3",
                    &json!({
                        "mode": "retrieve",
                        "chunk_manifest": [{
                            "chunk_id": "chunk_003",
                            "source_id": "r3",
                            "approx_bytes": 3000
                        }]
                    })
                    .to_string(),
                ),
            ]
        );
        assert_eq!(output.len(), 4);
    }

    #[test]
    fn collect_manage_context_seed_items_keeps_latest_eligible_pair_when_newer_same_mode_is_incomplete()
     {
        let retrieve_output = json!({
            "mode": "retrieve",
            "chunk_manifest": [{
                "chunk_id": "chunk_001",
                "source_id": "r1",
                "approx_bytes": 1000
            }]
        })
        .to_string();
        let items = vec![
            manage_context_call("call-1"),
            function_call_output("call-1", &retrieve_output),
            manage_context_call("call-2"),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(
            output,
            vec![
                manage_context_call("call-1"),
                function_call_output("call-1", &retrieve_output),
            ]
        );
    }

    #[test]
    fn collect_manage_context_seed_items_ignores_malformed_and_error_outputs() {
        let retrieve_output = json!({
            "mode": "retrieve",
            "chunk_manifest": [{
                "chunk_id": "chunk_001",
                "source_id": "r1",
                "approx_bytes": 1000
            }]
        })
        .to_string();
        let items = vec![
            manage_context_call("retrieve-ok"),
            function_call_output("retrieve-ok", &retrieve_output),
            manage_context_call("bad-json"),
            function_call_output("bad-json", "{not-json"),
            manage_context_call("error"),
            function_call_output(
                "error",
                &json!({
                    "stop_reason": "state_hash_mismatch",
                    "message": "state_hash mismatch"
                })
                .to_string(),
            ),
            manage_context_call("unknown"),
            function_call_output(
                "unknown",
                &json!({
                    "mode": "unexpected"
                })
                .to_string(),
            ),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(
            output,
            vec![
                manage_context_call("retrieve-ok"),
                function_call_output("retrieve-ok", &retrieve_output),
            ]
        );
    }

    #[test]
    fn collect_manage_context_seed_items_selects_latest_pair_instance_for_reused_call_id() {
        let apply_output = json!({
            "mode": "apply",
            "stop_reason": "target_reached"
        })
        .to_string();
        let older_call = manage_context_call_with_arguments(
            "shared",
            &json!({
                "mode": "apply",
                "plan_id": "plan-old"
            })
            .to_string(),
        );
        let newer_call = manage_context_call_with_arguments(
            "shared",
            &json!({
                "mode": "apply",
                "plan_id": "plan-new"
            })
            .to_string(),
        );
        let items = vec![
            older_call,
            function_call_output("shared", &apply_output),
            newer_call.clone(),
            function_call_output("shared", &apply_output),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(
            output,
            vec![newer_call, function_call_output("shared", &apply_output),]
        );
    }

    #[test]
    fn collect_manage_context_seed_items_supports_custom_tool_variant() {
        let retrieve_input = json!({
            "mode": "retrieve",
            "policy_id": "sanitize_strict"
        })
        .to_string();
        let retrieve_output = json!({
            "mode": "retrieve",
            "chunk_manifest": [{
                "chunk_id": "chunk_001",
                "source_id": "r-custom",
                "approx_bytes": 1000
            }]
        })
        .to_string();
        let items = vec![
            custom_manage_context_call("custom-1", &retrieve_input),
            custom_manage_context_output("custom-1", &retrieve_output),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(output, items);
    }

    #[test]
    fn collect_manage_context_seed_items_ignores_ambiguous_call_id_collision() {
        let items = vec![
            manage_context_call("shared"),
            non_manage_call("shared"),
            function_call_output(
                "shared",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let output = collect_manage_context_seed_items(&items);
        assert!(output.is_empty());
    }

    #[test]
    fn manage_context_follow_up_events_detect_apply_mode() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let event = manage_context_follow_up_events_from_items(&items)
            .into_iter()
            .last();

        assert_eq!(event, Some(ManageContextFollowUpEvent::Apply));
    }

    #[test]
    fn manage_context_follow_up_events_detect_error_stop_reason_without_mode() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "stop_reason": "state_hash_mismatch",
                    "message": "state_hash mismatch (expected old, got new)"
                })
                .to_string(),
            ),
        ];

        let event = manage_context_follow_up_events_from_items(&items)
            .into_iter()
            .last();

        assert_eq!(
            event,
            Some(ManageContextFollowUpEvent::Error(
                ManageContextErrorSignature {
                    stop_reason: "state_hash_mismatch".to_string(),
                    message: Some("state_hash mismatch (expected old, got new)".to_string()),
                }
            ))
        );
    }

    #[test]
    fn manage_context_follow_up_events_extract_retrieve_signature() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [
                        {
                            "chunk_id": "chunk_001",
                            "source_id": "r42",
                            "approx_bytes": 8000
                        },
                        {
                            "chunk_id": "chunk_002",
                            "call_id": "call-heavy-b",
                            "approx_bytes": 4000
                        }
                    ]
                })
                .to_string(),
            ),
        ];

        let event = manage_context_follow_up_events_from_items(&items)
            .into_iter()
            .last();

        assert_eq!(
            event,
            Some(ManageContextFollowUpEvent::Retrieve(RetrieveSignature {
                chunk_fingerprints: vec![
                    "source:r42|8000".to_string(),
                    "call:call-heavy-b|4000".to_string(),
                ],
            }))
        );
    }

    #[test]
    fn manage_context_follow_up_events_since_detects_retrieve_when_history_shrinks() {
        let before = vec![
            input_text_message("user", "baseline"),
            manage_context_call("old-retrieve"),
            function_call_output(
                "old-retrieve",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r-old",
                        "approx_bytes": 5000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("old-apply"),
            function_call_output(
                "old-apply",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let after = vec![
            input_text_message("user", "baseline"),
            manage_context_call("new-retrieve"),
            function_call_output(
                "new-retrieve",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_900",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
        ];

        assert!(after.len() < before.len());

        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(
            events,
            vec![ManageContextFollowUpEvent::Retrieve(RetrieveSignature {
                chunk_fingerprints: vec!["source:r42|8000".to_string()],
            })]
        );

        let mut tracker = SanitizeStagnationTracker::new(2, 1);
        assert_eq!(
            tracker.record_follow_up_events(&events),
            SanitizeLoopDecision::Continue
        );
    }

    #[test]
    fn manage_context_follow_up_events_since_ignores_preexisting_outputs() {
        let before = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
        ];

        let after = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("call-2"),
            function_call_output(
                "call-2",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(events, vec![ManageContextFollowUpEvent::Apply]);
    }

    #[test]
    fn manage_context_follow_up_events_since_handles_reused_manage_call_id() {
        let before = vec![
            manage_context_call("call-reused"),
            function_call_output(
                "call-reused",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r-old",
                        "approx_bytes": 5000
                    }]
                })
                .to_string(),
            ),
        ];

        let after = vec![
            manage_context_call("call-reused"),
            function_call_output(
                "call-reused",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r-new",
                        "approx_bytes": 7000
                    }]
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(
            events,
            vec![ManageContextFollowUpEvent::Retrieve(RetrieveSignature {
                chunk_fingerprints: vec!["source:r-new|7000".to_string()],
            })]
        );
    }

    #[test]
    fn manage_context_follow_up_events_since_keeps_repeated_identical_output_for_reused_call_id() {
        let before = vec![
            manage_context_call("call-reused"),
            function_call_output(
                "call-reused",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let after = vec![
            manage_context_call("call-reused"),
            function_call_output(
                "call-reused",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
            manage_context_call("call-reused"),
            function_call_output(
                "call-reused",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(events, vec![ManageContextFollowUpEvent::Apply]);
    }

    #[test]
    fn manage_context_follow_up_events_since_ignores_local_shell_collision_output() {
        let before = vec![manage_context_call("call-1")];
        let after = vec![
            manage_context_call("shared"),
            local_shell_call("shared"),
            function_call_output(
                "shared",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_since(&before, &after);
        assert!(events.is_empty());
    }

    #[test]
    fn manage_context_follow_up_events_since_ignores_non_manage_outputs_with_mode_shape() {
        let before = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
        ];

        let after = vec![
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
            non_manage_call("tool-1"),
            function_call_output(
                "tool-1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "fake",
                        "source_id": "r-fake",
                        "approx_bytes": 9999
                    }]
                })
                .to_string(),
            ),
            manage_context_call("call-2"),
            function_call_output(
                "call-2",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(events, vec![ManageContextFollowUpEvent::Apply]);
    }

    #[test]
    fn manage_context_follow_up_events_since_detects_apply_when_history_shrinks() {
        let before = vec![
            input_text_message("user", "baseline"),
            manage_context_call("old-retrieve"),
            function_call_output(
                "old-retrieve",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r-old",
                        "approx_bytes": 5000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("old-apply"),
            function_call_output(
                "old-apply",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        let after = vec![
            input_text_message("user", "baseline"),
            manage_context_call("new-apply"),
            function_call_output(
                "new-apply",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
        ];

        assert!(after.len() < before.len());
        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(events, vec![ManageContextFollowUpEvent::Apply]);
    }

    #[test]
    fn manage_context_follow_up_events_since_detects_error_when_history_shrinks() {
        let before = vec![
            input_text_message("user", "baseline"),
            manage_context_call("old-retrieve"),
            function_call_output(
                "old-retrieve",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r-old",
                        "approx_bytes": 5000
                    }]
                })
                .to_string(),
            ),
        ];

        let after = vec![
            input_text_message("user", "baseline"),
            manage_context_call("new-apply"),
            function_call_output(
                "new-apply",
                &json!({
                    "stop_reason": "state_hash_mismatch",
                    "message": "state_hash mismatch (expected old, got new)"
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_since(&before, &after);
        assert_eq!(
            events,
            vec![ManageContextFollowUpEvent::Error(
                ManageContextErrorSignature {
                    stop_reason: "state_hash_mismatch".to_string(),
                    message: Some("state_hash mismatch (expected old, got new)".to_string()),
                }
            )]
        );
    }

    #[test]
    fn sanitize_stagnation_tracker_detects_repeated_retrieve_cycles() {
        let signature = RetrieveSignature {
            chunk_fingerprints: vec!["source:r42|9000".to_string()],
        };
        let mut tracker = SanitizeStagnationTracker::new(2, 2);

        assert_eq!(
            tracker.record_follow_up_events(&[ManageContextFollowUpEvent::Retrieve(
                signature.clone()
            )]),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(&[ManageContextFollowUpEvent::Retrieve(
                signature.clone()
            )]),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(&[ManageContextFollowUpEvent::Retrieve(signature)]),
            SanitizeLoopDecision::Stalled
        );
    }

    #[test]
    fn sanitize_stagnation_tracker_reaches_fixed_point_on_empty_manifest() {
        let signature = RetrieveSignature {
            chunk_fingerprints: Vec::new(),
        };
        let mut tracker = SanitizeStagnationTracker::new(2, 3);

        assert_eq!(
            tracker.record_follow_up_events(&[ManageContextFollowUpEvent::Retrieve(
                signature.clone()
            )]),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(&[ManageContextFollowUpEvent::Retrieve(signature)]),
            SanitizeLoopDecision::ReachedFixedPoint
        );
    }

    #[test]
    fn sanitize_stagnation_tracker_stalls_after_repeated_idle_cycles() {
        let mut tracker = SanitizeStagnationTracker::new(2, 2);

        assert_eq!(
            tracker.record_follow_up_events(&[]),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(&[]),
            SanitizeLoopDecision::Stalled
        );
    }

    #[test]
    fn sanitize_stagnation_tracker_handles_multi_output_response() {
        let items = vec![
            manage_context_call("call-r1"),
            function_call_output(
                "call-r1",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("call-a1"),
            function_call_output(
                "call-a1",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
            manage_context_call("call-r2"),
            function_call_output(
                "call-r2",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
            manage_context_call("call-a2"),
            function_call_output(
                "call-a2",
                &json!({
                    "mode": "apply",
                    "stop_reason": "target_reached"
                })
                .to_string(),
            ),
            manage_context_call("call-r3"),
            function_call_output(
                "call-r3",
                &json!({
                    "mode": "retrieve",
                    "chunk_manifest": [{
                        "chunk_id": "chunk_001",
                        "source_id": "r42",
                        "approx_bytes": 8000
                    }]
                })
                .to_string(),
            ),
        ];

        let events = manage_context_follow_up_events_from_items(&items);
        let mut tracker = SanitizeStagnationTracker::new(2, 2);

        assert_eq!(
            tracker.record_follow_up_events(&events),
            SanitizeLoopDecision::Stalled
        );
    }

    #[test]
    fn sanitize_stagnation_tracker_stalls_after_repeated_manage_context_errors() {
        let mut tracker = SanitizeStagnationTracker::new(2, 2);
        let error_event = ManageContextFollowUpEvent::Error(ManageContextErrorSignature {
            stop_reason: "state_hash_mismatch".to_string(),
            message: Some("state_hash mismatch (expected old, got new)".to_string()),
        });

        assert_eq!(
            tracker.record_follow_up_events(std::slice::from_ref(&error_event)),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(std::slice::from_ref(&error_event)),
            SanitizeLoopDecision::Stalled
        );

        let message = tracker.stalled_loop_message();
        assert!(message.contains("state_hash_mismatch"));
        assert!(message.contains("No context updates were applied."));
        assert!(message.contains("Last manage_context error: state_hash mismatch"));
    }

    #[test]
    fn sanitize_stagnation_tracker_resets_error_stall_after_progress() {
        let mut tracker = SanitizeStagnationTracker::new(2, 2);
        let error_event = ManageContextFollowUpEvent::Error(ManageContextErrorSignature {
            stop_reason: "state_hash_mismatch".to_string(),
            message: Some("state_hash mismatch".to_string()),
        });
        let retrieve_event = ManageContextFollowUpEvent::Retrieve(RetrieveSignature {
            chunk_fingerprints: vec!["source:r42|1000".to_string()],
        });

        assert_eq!(
            tracker.record_follow_up_events(std::slice::from_ref(&error_event)),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(std::slice::from_ref(&retrieve_event)),
            SanitizeLoopDecision::Continue
        );
        assert_eq!(
            tracker.record_follow_up_events(std::slice::from_ref(&error_event)),
            SanitizeLoopDecision::Continue
        );
    }

    #[test]
    fn summarize_status_message_compacts_and_truncates() {
        let summarized = summarize_status_message(
            "  state_hash    mismatch   with many spaces and a long tail that should be trimmed",
            40,
        );
        assert_eq!(summarized, "state_hash mismatch with many spaces...");
        assert!(summarized.chars().count() <= 40);
    }

    #[test]
    fn summarize_status_message_handles_small_limits() {
        assert_eq!(summarize_status_message("abcdef", 0), "");
        assert_eq!(summarize_status_message("abcdef", 1), ".");
        assert_eq!(summarize_status_message("abcdef", 2), "..");
        assert_eq!(summarize_status_message("abcdef", 3), "...");
    }

    #[test]
    fn sanitize_completion_message_prefers_model_output() {
        let message = sanitize_completion_message(Some("done".to_string()), true);
        assert_eq!(message, "done");
    }

    #[test]
    fn sanitize_completion_message_reports_changes_when_model_is_silent() {
        let message = sanitize_completion_message(None, true);
        assert_eq!(message, SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE);
    }

    #[test]
    fn sanitize_completion_message_reports_no_changes_when_model_is_silent() {
        let message = sanitize_completion_message(None, false);
        assert_eq!(message, SANITIZE_COMPLETED_NO_CHANGES_MESSAGE);
    }

    #[test]
    fn sanitize_completion_message_ignores_model_output_when_semantically_unchanged() {
        let message = sanitize_completion_message(
            Some("completed and applied context updates".to_string()),
            false,
        );
        assert_eq!(message, SANITIZE_COMPLETED_NO_CHANGES_MESSAGE);
    }

    #[test]
    fn sanitize_completion_message_treats_whitespace_as_silent() {
        let message = sanitize_completion_message(Some(" \n\t".to_string()), true);
        assert_eq!(message, SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_returns_none_when_no_changes() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let items = [input_text_message("user", "hello")];
        state.record_items(
            items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );

        let baseline = state.history_snapshot_lenient();
        let materialized = sanitize_replacement_history_if_changed(&mut state, &baseline, &[]);

        assert_eq!(materialized, None);
        assert_eq!(state.history_snapshot_lenient(), baseline);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_replaces_history_and_clears_overrides() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let items = [
            input_text_message("user", "first"),
            input_text_message("assistant", "second"),
        ];
        state.record_items(
            items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let baseline = state.history_snapshot_lenient();

        state.set_context_inclusion(&[1], false);
        let first_rid = state.history_rids_snapshot_lenient()[0];
        state.upsert_context_replacements(vec![(first_rid, "first replaced".to_string())]);
        state.add_context_notes(vec!["Decision: keep strict pruning.".to_string()]);

        let expected = state.prompt_snapshot_lenient();
        let materialized = sanitize_replacement_history_if_changed(&mut state, &baseline, &[])
            .expect("must materialize");

        assert!(materialized.history_cleanup_required);
        assert!(materialized.semantic_context_changed);
        assert_eq!(materialized.replacement_history, expected);
        assert_eq!(state.history_snapshot_lenient(), expected);

        let overlay = state.context_overlay_snapshot();
        assert!(overlay.replacements_by_rid.is_empty());
        assert!(overlay.notes.is_empty());
        assert!(
            state
                .build_context_items_event()
                .items
                .iter()
                .all(|item| item.included)
        );
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_persists_delete_only_changes() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let items = [
            input_text_message("user", "first"),
            input_text_message("assistant", "second"),
        ];
        state.record_items(
            items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let baseline = state.history_snapshot_lenient();

        state.replace_history(vec![input_text_message("assistant", "second")], None);

        let expected = state.history_snapshot_lenient();
        let materialized = sanitize_replacement_history_if_changed(&mut state, &baseline, &[])
            .expect("delete-only change should persist");

        assert!(!materialized.history_cleanup_required);
        assert!(materialized.semantic_context_changed);
        assert_eq!(materialized.replacement_history, expected);
        assert_eq!(state.history_snapshot_lenient(), expected);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_drops_completed_manage_context_pairs_for_cleanup_only_runs()
     {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let baseline = [input_text_message("user", "baseline")];
        state.record_items(
            baseline.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let baseline_snapshot = state.history_snapshot_lenient();

        let extra_items = [
            manage_context_call("call-1"),
            function_call_output(
                "call-1",
                &json!({
                    "mode": "retrieve",
                    "plan_id": "p1"
                })
                .to_string(),
            ),
            input_text_message(
                "assistant",
                "/sanitize completed and applied context updates.",
            ),
        ];
        state.record_items(
            extra_items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );

        let sanitize_generated_non_tool_items = [input_text_message(
            "assistant",
            "/sanitize completed and applied context updates.",
        )];
        let materialized = sanitize_replacement_history_if_changed(
            &mut state,
            &baseline_snapshot,
            &sanitize_generated_non_tool_items,
        )
        .expect("cleanup-only sanitize chatter must materialize");

        assert!(materialized.history_cleanup_required);
        assert!(!materialized.semantic_context_changed);
        assert_eq!(materialized.replacement_history, baseline_snapshot);
        assert_eq!(state.history_snapshot_lenient(), baseline_snapshot);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_preserves_in_flight_manage_context_calls() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let baseline = [
            input_text_message("user", "baseline"),
            non_manage_call("tool-1"),
            function_call_output("tool-1", "ok"),
        ];
        state.record_items(
            baseline.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let baseline_snapshot = state.history_snapshot_lenient();

        let extra_items = [
            manage_context_call("done"),
            function_call_output("done", "{\"mode\":\"retrieve\"}"),
            manage_context_call("in-flight"),
        ];
        state.record_items(
            extra_items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        state.set_context_inclusion(&[0], false);

        let materialized =
            sanitize_replacement_history_if_changed(&mut state, &baseline_snapshot, &[])
                .expect("must materialize with in-flight manage_context call preserved");

        assert!(materialized.history_cleanup_required);
        assert!(materialized.semantic_context_changed);
        assert!(materialized.replacement_history.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "in-flight"
            )
        }));
        assert!(!materialized.replacement_history.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall { name, call_id, .. }
                    if name == "manage_context" && call_id == "done"
            )
        }));
        assert!(!materialized.replacement_history.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. }
                    if call_id == "done" || call_id == "in-flight"
            )
        }));
        assert_eq!(
            state.history_snapshot_lenient(),
            materialized.replacement_history
        );
    }

    #[tokio::test]
    async fn sanitize_rollout_persist_failure_rolls_back_state() {
        let (session, turn) = crate::codex::make_session_and_context().await;

        let recorder = crate::rollout::RolloutRecorder::new(
            turn.config.as_ref(),
            crate::rollout::RolloutRecorderParams::new(
                session.conversation_id,
                None,
                turn.session_source.clone(),
                codex_protocol::models::BaseInstructions::default(),
                Vec::new(),
                crate::rollout::policy::EventPersistenceMode::Limited,
            ),
            None,
            None,
        )
        .await
        .expect("create rollout recorder");
        recorder
            .shutdown()
            .await
            .expect("shutdown rollout recorder");
        {
            let mut guard = session.services.rollout.lock().await;
            *guard = Some(recorder);
        }

        let history_before_sanitize = {
            let mut state = session.state.lock().await;
            let items = [
                input_text_message("user", "first"),
                input_text_message("assistant", "second"),
            ];
            state.record_items(items.iter(), turn.truncation_policy);
            let baseline = state.history_snapshot_lenient();
            state.set_context_inclusion(&[1], false);
            let first_rid = state.history_rids_snapshot_lenient()[0];
            state.upsert_context_replacements(vec![(first_rid, "first replaced".to_string())]);
            state.add_context_notes(vec!["Decision: keep strict pruning.".to_string()]);
            baseline
        };

        let (before_history, before_overlay, before_context_items, before_rids) = {
            let state = session.state.lock().await;
            (
                state.history_snapshot_lenient(),
                state.context_overlay_snapshot(),
                state.build_context_items_event(),
                state.history_rids_snapshot_lenient(),
            )
        };

        let error =
            materialize_sanitize_history_if_changed(&session, &turn, &history_before_sanitize, &[])
                .await
                .expect_err("materialization should fail when rollout persistence cannot enqueue");
        assert!(
            error
                .to_string()
                .contains("failed to persist compacted replacement_history"),
            "unexpected error: {error}"
        );

        let (after_history, after_overlay, after_context_items, after_rids) = {
            let state = session.state.lock().await;
            (
                state.history_snapshot_lenient(),
                state.context_overlay_snapshot(),
                state.build_context_items_event(),
                state.history_rids_snapshot_lenient(),
            )
        };
        assert_eq!(after_history, before_history);
        assert_eq!(after_overlay, before_overlay);
        assert_eq!(after_context_items, before_context_items);
        assert_eq!(after_rids, before_rids);
    }
}
