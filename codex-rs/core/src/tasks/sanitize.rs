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
use crate::turn_diff_tracker::TurnDiffTracker;

use super::SessionTask;
use super::SessionTaskContext;

const SANITIZE_CONTEXT_WINDOW_EXCEEDED_MESSAGE: &str = "/sanitize could not continue because the context window is still full. Run /compact, then retry /sanitize.";
const SANITIZE_ERROR_MESSAGE_PREFIX: &str = "/sanitize failed:";
const SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE: &str =
    "/sanitize completed and applied context updates.";
const SANITIZE_COMPLETED_NO_CHANGES_MESSAGE: &str = "/sanitize completed with no context changes.";

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
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let truncated = normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    format!("{truncated}...")
}

fn build_sanitize_prompt(policy: &crate::config::ManageContextPolicy) -> String {
    format!(
        "{SANITIZE_PROMPT}\n\nRuntime manage_context policy (authoritative):\n- policy_id: {}\n- fixed_point_k: {}\n- stalled_signature_threshold: {}\n- max_chunks_per_apply: {}",
        policy.quality_rubric_id,
        policy.fixed_point_k,
        policy.stalled_signature_threshold,
        policy.max_chunks_per_apply,
    )
}

async fn materialize_sanitize_history_if_changed(
    sess: &crate::codex::Session,
    ctx: &TurnContext,
    history_before_sanitize: &[ResponseItem],
) -> bool {
    let replacement_history = {
        let mut state = sess.state.lock().await;
        sanitize_replacement_history_if_changed(&mut state, history_before_sanitize)
    };
    let changed_history = replacement_history.is_some();
    if let Some(replacement_history) = replacement_history {
        let compacted_item = RolloutItem::Compacted(CompactedItem {
            message: String::new(),
            replacement_history: Some(replacement_history),
        });
        sess.persist_rollout_items(&[compacted_item]).await;
    }
    sess.recompute_token_usage(ctx).await;
    changed_history
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
        let allowed_tool_names = HashSet::from(["manage_context".to_string()]);
        let history_before_sanitize = {
            let state = sess.state.lock().await;
            state.history_snapshot_lenient()
        };
        let mut stagnation_tracker = SanitizeStagnationTracker::new(
            manage_context_policy.fixed_point_k,
            manage_context_policy.stalled_signature_threshold,
        );

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

            if !output.needs_follow_up {
                let changed_history = materialize_sanitize_history_if_changed(
                    sess.as_ref(),
                    ctx.as_ref(),
                    &history_before_sanitize,
                )
                .await;
                return Some(sanitize_completion_message(
                    output.last_agent_message,
                    changed_history,
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
                    let changed_history = materialize_sanitize_history_if_changed(
                        sess.as_ref(),
                        ctx.as_ref(),
                        &history_before_sanitize,
                    )
                    .await;
                    return Some(sanitize_completion_message(
                        output.last_agent_message,
                        changed_history,
                    ));
                }
                SanitizeLoopDecision::Stalled => {
                    let changed_history = materialize_sanitize_history_if_changed(
                        sess.as_ref(),
                        ctx.as_ref(),
                        &history_before_sanitize,
                    )
                    .await;
                    stagnation_tracker.set_changed_history(changed_history);
                    return Some(stagnation_tracker.stalled_loop_message());
                }
            }
        }
    }
}

fn sanitize_completion_message(
    last_agent_message: Option<String>,
    changed_history: bool,
) -> String {
    if let Some(message) = last_agent_message
        && !message.trim().is_empty()
    {
        return message;
    }

    if changed_history {
        SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE.to_string()
    } else {
        SANITIZE_COMPLETED_NO_CHANGES_MESSAGE.to_string()
    }
}

fn sanitize_replacement_history_if_changed(
    state: &mut crate::state::SessionState,
    history_before_sanitize: &[ResponseItem],
) -> Option<Vec<ResponseItem>> {
    let current_history = state.history_snapshot_lenient();
    let prompt_snapshot = state.prompt_snapshot_lenient();

    if current_history == history_before_sanitize && prompt_snapshot == history_before_sanitize {
        return None;
    }

    if prompt_snapshot != current_history {
        state.replace_history(prompt_snapshot.clone());
    }

    Some(prompt_snapshot)
}

fn parse_manage_context_output_item(item: &ResponseItem) -> Option<Value> {
    let text = match item {
        ResponseItem::FunctionCallOutput { output, .. } => output.body.to_text()?,
        ResponseItem::CustomToolCallOutput { output, .. } => output.clone(),
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

fn manage_context_call_ids(items: &[ResponseItem]) -> HashSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { name, call_id, .. }
            | ResponseItem::CustomToolCall { name, call_id, .. }
                if name == "manage_context" =>
            {
                Some(call_id.clone())
            }
            _ => None,
        })
        .collect()
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
    let before_manage_call_ids = manage_context_call_ids(before);
    let after_manage_call_ids = manage_context_call_ids(after);
    if after_manage_call_ids.is_empty() {
        return Vec::new();
    }

    let mut seen_output_signatures: HashSet<(String, String)> = before
        .iter()
        .filter_map(|item| {
            let (call_id, output_signature, _) = manage_context_output_signature(item)?;
            if !before_manage_call_ids.contains(call_id.as_str()) {
                return None;
            }
            Some((call_id, output_signature))
        })
        .collect();

    after
        .iter()
        .filter_map(|item| {
            let (call_id, output_signature, output_value) = manage_context_output_signature(item)?;
            if !after_manage_call_ids.contains(call_id.as_str()) {
                return None;
            }
            if !seen_output_signatures.insert((call_id, output_signature)) {
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

fn collect_manage_context_seed_items(items: &[ResponseItem]) -> Vec<ResponseItem> {
    let mut manage_context_call_ids: HashSet<String> = HashSet::new();
    let mut manage_context_output_ids: HashSet<String> = HashSet::new();

    for item in items {
        match item {
            ResponseItem::FunctionCall { name, call_id, .. } if name == "manage_context" => {
                manage_context_call_ids.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { name, call_id, .. } if name == "manage_context" => {
                manage_context_call_ids.insert(call_id.clone());
            }
            ResponseItem::FunctionCallOutput { call_id, .. }
            | ResponseItem::CustomToolCallOutput { call_id, .. } => {
                manage_context_output_ids.insert(call_id.clone());
            }
            _ => {}
        }
    }

    let completed_call_ids: HashSet<String> = manage_context_call_ids
        .into_iter()
        .filter(|call_id| manage_context_output_ids.contains(call_id))
        .collect();
    if completed_call_ids.is_empty() {
        return Vec::new();
    }

    items
        .iter()
        .filter(|item| match item {
            ResponseItem::FunctionCall { name, call_id, .. }
            | ResponseItem::CustomToolCall { name, call_id, .. } => {
                name == "manage_context" && completed_call_ids.contains(call_id)
            }
            ResponseItem::FunctionCallOutput { call_id, .. }
            | ResponseItem::CustomToolCallOutput { call_id, .. } => {
                completed_call_ids.contains(call_id)
            }
            _ => false,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_configuration_for_tests;
    use codex_protocol::models::ContentItem;
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

    #[test]
    fn collect_manage_context_seed_items_includes_all_complete_pairs_in_order() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output("call-1", "out-1"),
            manage_context_call("call-2"),
            function_call_output("call-2", "out-2"),
            manage_context_call("call-3"),
            function_call_output("call-3", "out-3"),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(
            output,
            vec![
                manage_context_call("call-1"),
                function_call_output("call-1", "out-1"),
                manage_context_call("call-2"),
                function_call_output("call-2", "out-2"),
                manage_context_call("call-3"),
                function_call_output("call-3", "out-3")
            ]
        );
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
            function_call_output("call-1", "out-1"),
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
    fn collect_manage_context_seed_items_prefers_older_complete_pairs_when_recent_are_incomplete() {
        let items = vec![
            manage_context_call("call-1"),
            function_call_output("call-1", "out-1"),
            manage_context_call("call-2"),
        ];

        let output = collect_manage_context_seed_items(&items);

        assert_eq!(
            output,
            vec![
                manage_context_call("call-1"),
                function_call_output("call-1", "out-1"),
            ]
        );
    }

    #[test]
    fn collect_manage_context_seed_items_includes_all_complete_calls_without_limit() {
        let mut items = Vec::new();
        for idx in 1..=12 {
            let call_id = format!("call-{idx}");
            let output = format!("out-{idx}");
            items.push(manage_context_call(&call_id));
            items.push(function_call_output(&call_id, &output));
        }

        let output = collect_manage_context_seed_items(&items);

        let expected = (1..=12)
            .flat_map(|idx| {
                let call_id = format!("call-{idx}");
                let output = format!("out-{idx}");
                [
                    manage_context_call(&call_id),
                    function_call_output(&call_id, &output),
                ]
            })
            .collect::<Vec<ResponseItem>>();

        assert_eq!(output, expected);
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
        assert_eq!(summarized, "state_hash mismatch with many spaces an...");
    }

    #[test]
    fn sanitize_completion_message_prefers_model_output() {
        let message = sanitize_completion_message(Some("done".to_string()), false);
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
    fn sanitize_completion_message_treats_whitespace_as_silent() {
        let message = sanitize_completion_message(Some(" \n\t".to_string()), true);
        assert_eq!(message, SANITIZE_COMPLETED_WITH_CHANGES_MESSAGE);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_returns_none_when_no_changes() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let items = vec![input_text_message("user", "hello")];
        state.record_items(
            items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );

        let baseline = state.history_snapshot_lenient();
        let materialized = sanitize_replacement_history_if_changed(&mut state, &baseline);

        assert_eq!(materialized, None);
        assert_eq!(state.history_snapshot_lenient(), baseline);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_replaces_history_and_clears_overrides() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let items = vec![
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
        let materialized = sanitize_replacement_history_if_changed(&mut state, &baseline)
            .expect("must materialize");

        assert_eq!(materialized, expected);
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
        let items = vec![
            input_text_message("user", "first"),
            input_text_message("assistant", "second"),
        ];
        state.record_items(
            items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let baseline = state.history_snapshot_lenient();

        state.replace_history(vec![input_text_message("assistant", "second")]);

        let expected = state.history_snapshot_lenient();
        let replacement_history = sanitize_replacement_history_if_changed(&mut state, &baseline)
            .expect("delete-only change should persist");

        assert_eq!(replacement_history, expected);
        assert_eq!(state.history_snapshot_lenient(), expected);
    }

    #[tokio::test]
    async fn sanitize_replacement_history_if_changed_keeps_manage_context_pairs() {
        let session_configuration = make_session_configuration_for_tests().await;
        let mut state = crate::state::SessionState::new(session_configuration);
        let baseline = vec![input_text_message("user", "baseline")];
        state.record_items(
            baseline.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let baseline_snapshot = state.history_snapshot_lenient();

        let extra_items = vec![
            manage_context_call("call-1"),
            function_call_output("call-1", "{\"mode\":\"retrieve\",\"plan_id\":\"p1\"}"),
        ];
        state.record_items(
            extra_items.iter(),
            crate::truncate::TruncationPolicy::Tokens(4_096),
        );
        let expected = state.history_snapshot_lenient();

        let replacement_history =
            sanitize_replacement_history_if_changed(&mut state, &baseline_snapshot)
                .expect("must materialize with manage_context pairs preserved");

        assert_eq!(replacement_history, expected);
        assert_eq!(state.history_snapshot_lenient(), expected);
    }
}
