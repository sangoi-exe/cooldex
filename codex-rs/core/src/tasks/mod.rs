mod compact;
mod ghost_snapshot;
mod regular;
mod review;
mod sanitize;
mod undo;
mod user_shell;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::select;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::Span;
use tracing::debug;
use tracing::trace;
use tracing::warn;

use crate::AuthManager;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::config;
use crate::instructions::SkillInstructions;
use crate::instructions::UserInstructions;
use crate::models_manager::manager::ModelsManager;
use crate::protocol::ContextInclusionItem;
use crate::protocol::ContextOverlayItem;
use crate::protocol::ContextOverlayReplacement;
use crate::protocol::EventMsg;
use crate::protocol::REASONING_CONTEXT_CLOSE_TAG;
use crate::protocol::REASONING_CONTEXT_OPEN_TAG;
use crate::protocol::TOOL_CONTEXT_CLOSE_TAG;
use crate::protocol::TOOL_CONTEXT_OPEN_TAG;
use crate::protocol::TurnAbortReason;
use crate::protocol::TurnAbortedEvent;
use crate::protocol::TurnCompleteEvent;
use crate::rid;
use crate::session_prefix::TURN_ABORTED_OPEN_TAG;
use crate::state::ActiveTurn;
use crate::state::RunningTask;
use crate::state::TaskKind;
use crate::truncate::TruncationPolicy;
use crate::truncate::truncate_text;
use codex_protocol::models::ContentItem;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::WebSearchAction;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::user_input::UserInput;
use serde_json::Value as JsonValue;

pub(crate) use compact::CompactTask;
pub(crate) use ghost_snapshot::GhostSnapshotTask;
pub(crate) use regular::RegularTask;
pub(crate) use review::ReviewTask;
pub(crate) use sanitize::SanitizeTask;
pub(crate) use undo::UndoTask;
pub(crate) use user_shell::UserShellCommandMode;
pub(crate) use user_shell::UserShellCommandTask;
pub(crate) use user_shell::execute_user_shell_command;

const GRACEFULL_INTERRUPTION_TIMEOUT_MS: u64 = 100;
const TURN_ABORTED_INTERRUPTED_GUIDANCE: &str = "The user interrupted the previous turn on purpose. If any tools/commands were aborted, they may have partially executed; verify current state before retrying.";

/// Thin wrapper that exposes the parts of [`Session`] task runners need.
#[derive(Clone)]
pub(crate) struct SessionTaskContext {
    session: Arc<Session>,
}

impl SessionTaskContext {
    pub(crate) fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    pub(crate) fn clone_session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }

    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.session.services.auth_manager)
    }

    pub(crate) fn models_manager(&self) -> Arc<ModelsManager> {
        Arc::clone(&self.session.services.models_manager)
    }
}

/// Async task that drives a [`Session`] turn.
///
/// Implementations encapsulate a specific Codex workflow (regular chat,
/// reviews, ghost snapshots, etc.). Each task instance is owned by a
/// [`Session`] and executed on a background Tokio task. The trait is
/// intentionally small: implementers identify themselves via
/// [`SessionTask::kind`], perform their work in [`SessionTask::run`], and may
/// release resources in [`SessionTask::abort`].
#[async_trait]
pub(crate) trait SessionTask: Send + Sync + 'static {
    /// Describes the type of work the task performs so the session can
    /// surface it in telemetry and UI.
    fn kind(&self) -> TaskKind;

    /// Executes the task until completion or cancellation.
    ///
    /// Implementations typically stream protocol events using `session` and
    /// `ctx`, returning an optional final agent message when finished. The
    /// provided `cancellation_token` is cancelled when the session requests an
    /// abort; implementers should watch for it and terminate quickly once it
    /// fires. Returning [`Some`] yields a final message that
    /// [`Session::on_task_finished`] will emit to the client.
    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String>;

    /// Gives the task a chance to perform cleanup after an abort.
    ///
    /// The default implementation is a no-op; override this if additional
    /// teardown or notifications are required once
    /// [`Session::abort_all_tasks`] cancels the task.
    async fn abort(&self, session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
        let _ = (session, ctx);
    }
}

impl Session {
    pub async fn spawn_task<T: SessionTask>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<UserInput>,
        task: T,
    ) {
        self.abort_all_tasks(TurnAbortReason::Replaced).await;
        self.seed_initial_context_if_needed(turn_context.as_ref())
            .await;

        let task: Arc<dyn SessionTask> = Arc::new(task);
        let task_kind = task.kind();

        let cancellation_token = CancellationToken::new();
        let done = Arc::new(Notify::new());

        let done_clone = Arc::clone(&done);
        let handle = {
            let session_ctx = Arc::new(SessionTaskContext::new(Arc::clone(self)));
            let ctx = Arc::clone(&turn_context);
            let task_for_run = Arc::clone(&task);
            let task_cancellation_token = cancellation_token.child_token();
            let session_span = Span::current();
            tokio::spawn(
                async move {
                    let ctx_for_finish = Arc::clone(&ctx);
                    let last_agent_message = task_for_run
                        .run(
                            Arc::clone(&session_ctx),
                            ctx,
                            input,
                            task_cancellation_token.child_token(),
                        )
                        .await;
                    session_ctx.clone_session().flush_rollout().await;
                    if !task_cancellation_token.is_cancelled() {
                        // Emit completion uniformly from spawn site so all tasks share the same lifecycle.
                        let sess = session_ctx.clone_session();
                        sess.on_task_finished(ctx_for_finish, last_agent_message)
                            .await;
                    }
                    done_clone.notify_waiters();
                }
                .instrument(session_span),
            )
        };

        let timer = turn_context
            .otel_manager
            .start_timer("codex.turn.e2e_duration_ms", &[])
            .ok();

        let running_task = RunningTask {
            done,
            handle: Arc::new(AbortOnDropHandle::new(handle)),
            kind: task_kind,
            task,
            cancellation_token,
            turn_context: Arc::clone(&turn_context),
            _timer: timer,
        };
        self.register_new_active_task(running_task).await;
    }

    pub async fn abort_all_tasks(self: &Arc<Self>, reason: TurnAbortReason) {
        for task in self.take_all_running_tasks().await {
            self.handle_task_abort(task, reason.clone()).await;
        }
        self.close_unified_exec_processes().await;
    }

    pub async fn on_task_finished(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        last_agent_message: Option<String>,
    ) {
        let mut finished_kind = None;
        let mut pending_input = Vec::<ResponseInputItem>::new();
        let mut should_close_processes = false;

        {
            let mut active = self.active_turn.lock().await;
            if let Some(mut at) = active.take() {
                if let Some(task) = at.remove_task(&turn_context.sub_id) {
                    finished_kind = Some(task.kind);
                    should_close_processes = at.tasks.is_empty();
                    if should_close_processes {
                        let mut ts = at.turn_state.lock().await;
                        pending_input = ts.take_pending_input();
                    } else {
                        *active = Some(at);
                    }
                } else {
                    *active = Some(at);
                }
            }
        }

        if !pending_input.is_empty() {
            let pending_response_items = pending_input
                .into_iter()
                .map(ResponseItem::from)
                .collect::<Vec<_>>();
            self.record_conversation_items(turn_context.as_ref(), &pending_response_items)
                .await;
        }
        if should_close_processes {
            self.close_unified_exec_processes().await;
        }

        let should_auto_hygiene =
            finished_kind == Some(TaskKind::Regular) && last_agent_message.is_some();
        let event = EventMsg::TurnComplete(TurnCompleteEvent { last_agent_message });
        self.send_event(turn_context.as_ref(), event).await;

        if should_auto_hygiene {
            let cfg = &turn_context.config;
            if config::auto_sanitize_enabled(&cfg.codex_home, cfg.active_profile.as_deref()) {
                let sess = Arc::clone(self);
                tokio::spawn(async move {
                    if sess.active_turn.lock().await.is_some() {
                        return;
                    }
                    run_context_hygiene_pass(sess).await;
                });
            }
        }
    }

    async fn register_new_active_task(&self, task: RunningTask) {
        let mut active = self.active_turn.lock().await;
        let mut turn = ActiveTurn::default();
        turn.add_task(task);
        *active = Some(turn);
    }

    async fn take_all_running_tasks(&self) -> Vec<RunningTask> {
        let mut active = self.active_turn.lock().await;
        match active.take() {
            Some(mut at) => {
                at.clear_pending().await;

                at.drain_tasks()
            }
            None => Vec::new(),
        }
    }

    async fn close_unified_exec_processes(&self) {
        self.services
            .unified_exec_manager
            .terminate_all_processes()
            .await;
    }

    async fn handle_task_abort(self: &Arc<Self>, task: RunningTask, reason: TurnAbortReason) {
        let sub_id = task.turn_context.sub_id.clone();
        if task.cancellation_token.is_cancelled() {
            return;
        }

        trace!(task_kind = ?task.kind, sub_id, "aborting running task");
        task.cancellation_token.cancel();
        let session_task = task.task;

        select! {
            _ = task.done.notified() => {
            },
            _ = tokio::time::sleep(Duration::from_millis(GRACEFULL_INTERRUPTION_TIMEOUT_MS)) => {
                warn!("task {sub_id} didn't complete gracefully after {}ms", GRACEFULL_INTERRUPTION_TIMEOUT_MS);
            }
        }

        task.handle.abort();

        let session_ctx = Arc::new(SessionTaskContext::new(Arc::clone(self)));
        session_task
            .abort(session_ctx, Arc::clone(&task.turn_context))
            .await;

        if reason == TurnAbortReason::Interrupted {
            let marker = ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: format!(
                        "{TURN_ABORTED_OPEN_TAG}\n{TURN_ABORTED_INTERRUPTED_GUIDANCE}\n</turn_aborted>"
                    ),
                }],
                end_turn: None,
                phase: None,
            };
            self.record_into_history(std::slice::from_ref(&marker), task.turn_context.as_ref())
                .await;
            self.persist_rollout_items(&[RolloutItem::ResponseItem(marker)])
                .await;
            // Ensure the marker is durably visible before emitting TurnAborted: some clients
            // synchronously re-read the rollout on receipt of the abort event.
            self.flush_rollout().await;
        }

        let event = EventMsg::TurnAborted(TurnAbortedEvent { reason });
        self.send_event(task.turn_context.as_ref(), event).await;
    }
}

async fn run_context_hygiene_pass(sess: Arc<Session>) {
    let mut rollout_items = Vec::new();

    {
        let mut state = sess.state_lock().await;
        let (items, rids) = state.history_snapshot_with_rids_lenient();

        let Some(last_user_idx) = last_user_message_index(&items) else {
            return;
        };
        let Some(last_assistant_idx) = last_assistant_message_index(&items) else {
            return;
        };
        if last_user_idx >= last_assistant_idx {
            return;
        }

        // Build a tool-context summary from the tool calls in the last turn.
        let (tool_summaries, tool_indices) =
            summarize_tool_calls(&items, last_user_idx + 1..last_assistant_idx);

        // Consolidate included reasoning into a single note and exclude originals.
        let mut included_reasoning_indices = Vec::new();
        let mut included_reasoning_rids = Vec::new();
        for item in state.build_context_items_event().items {
            if item.category != crate::state::PruneCategory::Reasoning || !item.included {
                continue;
            }
            included_reasoning_indices.push(item.index);
            if let Some(id) = item.id.as_deref()
                && let Some(rid) = crate::rid::parse_rid(id)
            {
                included_reasoning_rids.push(rid);
            }
        }

        if tool_indices.is_empty() && included_reasoning_indices.is_empty() {
            return;
        }

        let tool_note = upsert_tagged_note(
            state.context_overlay_snapshot().notes.as_slice(),
            TOOL_CONTEXT_OPEN_TAG,
            TOOL_CONTEXT_CLOSE_TAG,
            tool_summaries.as_slice(),
            TruncationPolicy::Tokens(1_024),
        );

        let reasoning_note = if !included_reasoning_indices.is_empty() {
            let reasoning_note = build_reasoning_context_note(
                &items,
                &rids,
                &included_reasoning_rids,
                TruncationPolicy::Tokens(1_024),
            );
            state.set_context_inclusion(&included_reasoning_indices, false);
            Some(reasoning_note)
        } else {
            None
        };

        // Upsert notes (keep other notes intact).
        if let Some(tool_note) = tool_note {
            remove_notes_with_prefix(&mut state, TOOL_CONTEXT_OPEN_TAG);
            state.add_context_notes(vec![tool_note]);
        }
        if let Some(reasoning_note) = reasoning_note {
            remove_notes_with_prefix(&mut state, REASONING_CONTEXT_OPEN_TAG);
            state.add_context_notes(vec![reasoning_note]);
        }

        // Delete tool calls + outputs from the last turn (cascade by call_id).
        let prune = state.prune_by_indices_lenient(tool_indices);

        // Persist inclusion/deletion changes.
        let after_ev = state.build_context_items_event();
        let mut included_indices = Vec::new();
        let mut included_ids = Vec::new();
        for item in &after_ev.items {
            if item.included {
                included_indices.push(item.index);
                if let Some(id) = item.id.as_deref() {
                    included_ids.push(id.to_string());
                }
            }
        }

        if !prune.deleted_rids.is_empty() || !included_reasoning_indices.is_empty() {
            let deleted_ids = prune
                .deleted_rids
                .iter()
                .copied()
                .map(rid::rid_to_string)
                .collect();
            rollout_items.push(crate::protocol::RolloutItem::ContextInclusion(
                ContextInclusionItem {
                    included_indices,
                    included_ids,
                    deleted_indices: Vec::new(),
                    deleted_ids,
                },
            ));
        }

        // Persist overlay (notes).
        let overlay = state.context_overlay_snapshot();
        let mut replacements = Vec::new();
        for (rid, text) in &overlay.replacements_by_rid {
            replacements.push(ContextOverlayReplacement {
                id: rid::rid_to_string(*rid),
                text: text.clone(),
            });
        }
        rollout_items.push(crate::protocol::RolloutItem::ContextOverlay(
            ContextOverlayItem {
                replacements,
                notes: overlay.notes,
            },
        ));

        debug!(
            deleted = prune.deleted_rids.len(),
            "auto hygiene deleted tool items"
        );

        prune
    };

    if !rollout_items.is_empty() {
        sess.persist_rollout_items(&rollout_items).await;
    }
}

fn last_user_message_index(items: &[ResponseItem]) -> Option<usize> {
    items.iter().enumerate().rev().find_map(|(idx, item)| {
        let ResponseItem::Message { role, content, .. } = item else {
            return None;
        };
        if role != "user" {
            return None;
        }
        if UserInstructions::is_user_instructions(content)
            || SkillInstructions::is_skill_instructions(content)
        {
            return None;
        }
        if is_environment_context_message(content) {
            return None;
        }
        Some(idx)
    })
}

fn last_assistant_message_index(items: &[ResponseItem]) -> Option<usize> {
    items
        .iter()
        .rposition(|item| matches!(item, ResponseItem::Message { role, .. } if role == "assistant"))
}

fn is_environment_context_message(content: &[ContentItem]) -> bool {
    let Some(text) = first_text(content) else {
        return false;
    };
    text.trim()
        .get(..crate::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG.len())
        .is_some_and(|head| {
            head.eq_ignore_ascii_case(crate::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG)
        })
}

fn first_text(content: &[ContentItem]) -> Option<&str> {
    content.iter().find_map(|item| match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => Some(text.as_str()),
        ContentItem::InputImage { .. } => None,
    })
}

fn summarize_tool_calls(
    items: &[ResponseItem],
    range: std::ops::Range<usize>,
) -> (Vec<String>, Vec<usize>) {
    let mut summaries = Vec::new();
    let mut indices = Vec::new();

    for idx in range {
        let Some(item) = items.get(idx) else {
            continue;
        };
        match item {
            ResponseItem::FunctionCall {
                name, arguments, ..
            } => {
                indices.push(idx);
                if let Some(summary) = summarize_function_call(name, arguments) {
                    summaries.push(summary);
                }
            }
            ResponseItem::CustomToolCall { name, input, .. } => {
                indices.push(idx);
                summaries.push(summarize_custom_tool_call(name, input));
            }
            ResponseItem::LocalShellCall { action, .. } => {
                indices.push(idx);
                summaries.push(summarize_local_shell_call(action));
            }
            ResponseItem::WebSearchCall { action, .. } => {
                indices.push(idx);
                summaries.push(
                    action
                        .as_ref()
                        .map(summarize_web_search_call)
                        .unwrap_or_else(|| "web_search".to_string()),
                );
            }
            ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. } => {
                indices.push(idx);
            }
            _ => {}
        }
    }

    summaries.sort();
    summaries.dedup();
    indices.sort_unstable();
    indices.dedup();

    (summaries, indices)
}

fn summarize_function_call(name: &str, arguments: &str) -> Option<String> {
    match name {
        "exec_command" => {
            let JsonValue::Object(obj) = serde_json::from_str(arguments).ok()? else {
                return Some("exec_command".to_string());
            };
            let cmd = obj.get("cmd").and_then(JsonValue::as_str).unwrap_or("");
            if cmd.trim().is_empty() {
                Some("exec_command".to_string())
            } else {
                Some(format!("exec_command: {}", cmd.trim()))
            }
        }
        "apply_patch" => {
            let files = patch_touched_files(arguments);
            if files.is_empty() {
                Some("apply_patch".to_string())
            } else {
                Some(format!("apply_patch: {}", files.join(", ")))
            }
        }
        other => Some(other.to_string()),
    }
}

fn summarize_custom_tool_call(name: &str, input: &str) -> String {
    match name {
        "apply_patch" => {
            let files = patch_touched_files(input);
            if files.is_empty() {
                "apply_patch".to_string()
            } else {
                format!("apply_patch: {}", files.join(", "))
            }
        }
        other => format!("custom_tool: {other}"),
    }
}

fn summarize_local_shell_call(action: &LocalShellAction) -> String {
    match action {
        LocalShellAction::Exec(exec) => {
            if exec.command.is_empty() {
                "local_shell".to_string()
            } else {
                format!("local_shell: {}", exec.command.join(" "))
            }
        }
    }
}

fn summarize_web_search_call(action: &WebSearchAction) -> String {
    match action {
        WebSearchAction::Search { query, queries } => {
            let trimmed_query = query
                .as_deref()
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    let first = queries
                        .as_ref()
                        .and_then(|items| {
                            items
                                .iter()
                                .map(String::as_str)
                                .map(str::trim)
                                .find(|item| !item.is_empty())
                        })
                        .unwrap_or_default()
                        .to_string();
                    if queries.as_ref().is_some_and(|items| {
                        items
                            .iter()
                            .map(String::as_str)
                            .map(str::trim)
                            .filter(|item| !item.is_empty())
                            .nth(1)
                            .is_some()
                    }) && !first.is_empty()
                    {
                        format!("{first} ...")
                    } else {
                        first
                    }
                });
            if trimmed_query.is_empty() {
                "web_search".to_string()
            } else {
                format!("web_search: {trimmed_query}")
            }
        }
        WebSearchAction::OpenPage { url } => {
            if let Some(url) = url.as_deref()
                && !url.trim().is_empty()
            {
                format!("web_search_open: {}", url.trim())
            } else {
                "web_search_open".to_string()
            }
        }
        WebSearchAction::FindInPage { url, pattern } => {
            let url = url.as_deref().unwrap_or("").trim();
            let pattern = pattern.as_deref().unwrap_or("").trim();
            if !url.is_empty() && !pattern.is_empty() {
                format!("web_search_find: {pattern} in {url}")
            } else if !pattern.is_empty() {
                format!("web_search_find: {pattern}")
            } else if !url.is_empty() {
                format!("web_search_find in {url}")
            } else {
                "web_search_find".to_string()
            }
        }
        WebSearchAction::Other => "web_search".to_string(),
    }
}

fn patch_touched_files(patch: &str) -> Vec<String> {
    const PREFIXES: [&str; 4] = [
        "*** Update File: ",
        "*** Add File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];

    let mut files = Vec::new();
    for line in patch.lines() {
        let line = line.trim();
        if let Some(prefix) = PREFIXES.iter().find(|prefix| line.starts_with(**prefix)) {
            let path = line.trim_start_matches(*prefix).trim();
            if !path.is_empty() {
                files.push(path.to_string());
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

fn upsert_tagged_note(
    notes: &[String],
    open_tag: &str,
    close_tag: &str,
    new_lines: &[String],
    policy: TruncationPolicy,
) -> Option<String> {
    if new_lines.is_empty() {
        return None;
    }

    let mut existing_body = notes
        .iter()
        .find_map(|note| extract_tagged_body(note, open_tag, close_tag))
        .unwrap_or_default();

    if !existing_body.trim().is_empty() {
        existing_body.push_str("\n\n");
    }

    existing_body.push_str("Turn tools:\n");
    for line in new_lines {
        existing_body.push_str("- ");
        existing_body.push_str(line.trim());
        existing_body.push('\n');
    }

    let body = truncate_text(existing_body.trim(), policy);
    Some(format!("{open_tag}\n{body}\n{close_tag}"))
}

fn extract_tagged_body(note: &str, open_tag: &str, close_tag: &str) -> Option<String> {
    let trimmed = note.trim();
    if !trimmed.starts_with(open_tag) {
        return None;
    }
    let without_open = trimmed.trim_start_matches(open_tag).trim();
    let body = without_open
        .strip_suffix(close_tag)
        .unwrap_or(without_open)
        .trim();
    Some(body.to_string())
}

fn remove_notes_with_prefix(state: &mut crate::state::SessionState, open_tag: &str) {
    let indices: Vec<usize> = state
        .context_overlay_snapshot()
        .notes
        .iter()
        .enumerate()
        .filter_map(|(idx, note)| {
            if note.trim().starts_with(open_tag) {
                Some(idx)
            } else {
                None
            }
        })
        .collect();
    if indices.is_empty() {
        return;
    }
    state.remove_context_notes(&indices);
}

fn build_reasoning_context_note(
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    included_reasoning_rids: &[u64],
    policy: TruncationPolicy,
) -> String {
    let rid_set: std::collections::HashSet<u64> = included_reasoning_rids.iter().copied().collect();
    let mut parts: Vec<String> = Vec::new();

    for (item, rid) in snapshot_items.iter().zip(snapshot_rids.iter().copied()) {
        if !rid_set.contains(&rid) {
            continue;
        }
        let ResponseItem::Reasoning { summary, .. } = item else {
            continue;
        };
        for entry in summary {
            let codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } = entry;
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            parts.push(trimmed.to_string());
        }
    }

    let body = if parts.is_empty() {
        "No reasoning summaries found.".to_string()
    } else {
        parts.join("\n\n")
    };

    let truncated = truncate_text(&body, policy);
    format!(
        "{REASONING_CONTEXT_OPEN_TAG}\n{}\n{REASONING_CONTEXT_CLOSE_TAG}",
        truncated.trim()
    )
}

#[cfg(test)]
mod tests {}
