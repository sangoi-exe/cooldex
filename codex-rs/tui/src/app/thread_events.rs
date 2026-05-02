//! Thread event buffering and replay state for the TUI app.
//!
//! This module owns the per-thread event store used when the TUI switches between the main
//! conversation, subagents, and side conversations. It keeps buffered app-server notifications,
//! pending interactive request replay state, active-turn tracking, saved composer state, and
//! prompt-GC replay state close together with the replay behavior that consumes them.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct ThreadEventSnapshot {
    pub(super) session: Option<ThreadSessionState>,
    pub(super) turns: Vec<Turn>,
    pub(super) events: Vec<ThreadBufferedEvent>,
    pub(super) input_state: Option<ThreadInputState>,
    pub(super) prompt_gc_active: bool,
    // Merge-safety anchor: prompt-GC thread replay must preserve the full post-GC usage split
    // outside the event ring so evicted TurnStarted events do not collapse total usage into
    // context-left math on restore.
    pub(super) prompt_gc_token_usage_info: Option<TokenUsageInfo>,
}

#[derive(Debug, Clone)]
pub(super) enum ThreadBufferedEvent {
    Notification(ServerNotification),
    Request(ServerRequest),
    HistoryEntryResponse(GetHistoryEntryResponseEvent),
    FeedbackSubmission(FeedbackThreadEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FeedbackThreadEvent {
    pub(super) category: FeedbackCategory,
    pub(super) include_logs: bool,
    pub(super) feedback_audience: FeedbackAudience,
    pub(super) result: Result<String, String>,
}

#[derive(Debug)]
pub(super) struct ThreadEventStore {
    pub(super) session: Option<ThreadSessionState>,
    pub(super) turns: Vec<Turn>,
    pub(super) buffer: VecDeque<ThreadBufferedEvent>,
    pub(super) pending_interactive_replay: PendingInteractiveReplayState,
    pub(super) active_turn_id: Option<String>,
    pub(super) input_state: Option<ThreadInputState>,
    pub(super) prompt_gc_active: bool,
    pub(super) prompt_gc_token_usage_info: Option<TokenUsageInfo>,
    pub(super) prompt_gc_completion_pending: bool,
    pub(super) prompt_gc_private_usage_closed: bool,
    pub(super) capacity: usize,
    pub(super) active: bool,
}

impl ThreadEventStore {
    pub(super) fn event_survives_session_refresh(event: &ThreadBufferedEvent) -> bool {
        matches!(
            event,
            ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(ServerNotification::HookStarted(_))
                | ThreadBufferedEvent::Notification(ServerNotification::HookCompleted(_))
                | ThreadBufferedEvent::FeedbackSubmission(_)
        )
    }

    pub(super) fn new(capacity: usize) -> Self {
        Self {
            session: None,
            turns: Vec::new(),
            buffer: VecDeque::new(),
            pending_interactive_replay: PendingInteractiveReplayState::default(),
            active_turn_id: None,
            input_state: None,
            prompt_gc_active: false,
            prompt_gc_token_usage_info: None,
            prompt_gc_completion_pending: false,
            prompt_gc_private_usage_closed: false,
            capacity,
            active: false,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn new_with_session(
        capacity: usize,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Self {
        let mut store = Self::new(capacity);
        store.session = Some(session);
        store.set_turns(turns);
        store
    }

    pub(super) fn set_session(&mut self, session: ThreadSessionState, turns: Vec<Turn>) {
        self.session = Some(session);
        self.set_turns(turns);
    }

    pub(super) fn rebase_buffer_after_session_refresh(&mut self) {
        self.buffer.retain(Self::event_survives_session_refresh);
    }

    pub(super) fn set_turns(&mut self, turns: Vec<Turn>) {
        self.active_turn_id = turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, TurnStatus::InProgress))
            .map(|turn| turn.id.clone());
        self.turns = turns;
    }

    pub(super) fn push_notification(&mut self, notification: ServerNotification) {
        self.pending_interactive_replay
            .note_server_notification(&notification);
        // Merge-safety anchor: per-thread prompt-GC replay state must clear on visible thread
        // token boundaries and fresh thread turns so restored private-context indicators never
        // leak across turns or survive after a visible token update.
        match &notification {
            ServerNotification::TurnStarted(turn) => {
                self.active_turn_id = Some(turn.turn.id.clone());
                self.prompt_gc_token_usage_info = None;
                self.prompt_gc_completion_pending = false;
                self.prompt_gc_private_usage_closed = false;
            }
            ServerNotification::TurnCompleted(turn) => {
                if self.active_turn_id.as_deref() == Some(turn.turn.id.as_str()) {
                    self.active_turn_id = None;
                }
            }
            ServerNotification::ThreadTokenUsageUpdated(_) => {
                if self.prompt_gc_completion_pending {
                    self.prompt_gc_token_usage_info = None;
                    self.prompt_gc_private_usage_closed = true;
                }
            }
            ServerNotification::ThreadClosed(_) => {
                self.active_turn_id = None;
            }
            _ => {}
        }
        self.buffer
            .push_back(ThreadBufferedEvent::Notification(notification));
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && let ThreadBufferedEvent::Request(request) = &removed
        {
            self.pending_interactive_replay
                .note_evicted_server_request(request);
        }
    }

    pub(super) fn push_request(&mut self, request: ServerRequest) {
        self.pending_interactive_replay
            .note_server_request(&request);
        self.buffer.push_back(ThreadBufferedEvent::Request(request));
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && let ThreadBufferedEvent::Request(request) = &removed
        {
            self.pending_interactive_replay
                .note_evicted_server_request(request);
        }
    }

    pub(super) fn pending_replay_requests(&self) -> Vec<ServerRequest> {
        self.buffer
            .iter()
            .filter_map(|event| match event {
                ThreadBufferedEvent::Request(request)
                    if self
                        .pending_interactive_replay
                        .should_replay_snapshot_request(request) =>
                {
                    Some(request.clone())
                }
                ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(_)
                | ThreadBufferedEvent::HistoryEntryResponse(_)
                | ThreadBufferedEvent::FeedbackSubmission(_) => None,
            })
            .collect()
    }

    pub(super) fn file_change_changes(
        &self,
        turn_id: &str,
        item_id: &str,
    ) -> Option<Vec<codex_app_server_protocol::FileUpdateChange>> {
        self.buffer
            .iter()
            .rev()
            .find_map(|event| match event {
                ThreadBufferedEvent::Notification(ServerNotification::ItemStarted(
                    notification,
                )) if turn_id_matches(turn_id, &notification.turn_id) => {
                    file_change_item_changes(&notification.item, item_id)
                }
                ThreadBufferedEvent::Notification(ServerNotification::ItemCompleted(
                    notification,
                )) if turn_id_matches(turn_id, &notification.turn_id) => {
                    file_change_item_changes(&notification.item, item_id)
                }
                ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(_)
                | ThreadBufferedEvent::HistoryEntryResponse(_)
                | ThreadBufferedEvent::FeedbackSubmission(_) => None,
            })
            .or_else(|| {
                self.turns
                    .iter()
                    .rev()
                    .filter(|turn| turn_id_matches(turn_id, &turn.id))
                    .flat_map(|turn| turn.items.iter().rev())
                    .find_map(|item| file_change_item_changes(item, item_id))
            })
    }

    pub(super) fn apply_thread_rollback(&mut self, response: &ThreadRollbackResponse) {
        self.turns = response.thread.turns.clone();
        self.buffer.clear();
        self.pending_interactive_replay = PendingInteractiveReplayState::default();
        self.active_turn_id = None;
    }

    pub(super) fn snapshot(&self) -> ThreadEventSnapshot {
        ThreadEventSnapshot {
            session: self.session.clone(),
            turns: self.turns.clone(),
            // Thread switches replay buffered events into a rebuilt ChatWidget. Only replay
            // interactive prompts that are still pending, or answered approvals/input will reappear.
            events: self
                .buffer
                .iter()
                .filter(|event| match event {
                    ThreadBufferedEvent::Request(request) => self
                        .pending_interactive_replay
                        .should_replay_snapshot_request(request),
                    ThreadBufferedEvent::Notification(_)
                    | ThreadBufferedEvent::HistoryEntryResponse(_)
                    | ThreadBufferedEvent::FeedbackSubmission(_) => true,
                })
                .cloned()
                .collect(),
            input_state: self.input_state.clone(),
            prompt_gc_active: self.prompt_gc_active,
            prompt_gc_token_usage_info: self.prompt_gc_token_usage_info.clone(),
        }
    }

    pub(super) fn note_outbound_op<T>(&mut self, op: T)
    where
        T: Into<AppCommand>,
    {
        self.pending_interactive_replay.note_outbound_op(op);
    }

    pub(super) fn op_can_change_pending_replay_state<T>(op: T) -> bool
    where
        T: Into<AppCommand>,
    {
        PendingInteractiveReplayState::op_can_change_state(op)
    }

    pub(super) fn has_pending_thread_approvals(&self) -> bool {
        self.pending_interactive_replay
            .has_pending_thread_approvals()
    }

    pub(super) fn side_parent_pending_status(&self) -> Option<SideParentStatus> {
        if self
            .pending_interactive_replay
            .has_pending_thread_user_input()
        {
            Some(SideParentStatus::NeedsInput)
        } else if self
            .pending_interactive_replay
            .has_pending_thread_approvals()
        {
            Some(SideParentStatus::NeedsApproval)
        } else {
            None
        }
    }

    pub(super) fn active_turn_id(&self) -> Option<&str> {
        self.active_turn_id.as_deref()
    }

    pub(super) fn clear_active_turn_id(&mut self) {
        self.active_turn_id = None;
    }
}

fn turn_id_matches(request_turn_id: &str, candidate_turn_id: &str) -> bool {
    request_turn_id.is_empty() || request_turn_id == candidate_turn_id
}

fn file_change_item_changes(
    item: &ThreadItem,
    item_id: &str,
) -> Option<Vec<codex_app_server_protocol::FileUpdateChange>> {
    match item {
        ThreadItem::FileChange { id, changes, .. } if id == item_id => Some(changes.clone()),
        _ => None,
    }
}

#[derive(Debug)]
pub(super) struct ThreadEventChannel {
    pub(super) sender: mpsc::Sender<ThreadBufferedEvent>,
    pub(super) receiver: Option<mpsc::Receiver<ThreadBufferedEvent>>,
    pub(super) store: Arc<Mutex<ThreadEventStore>>,
}

impl ThreadEventChannel {
    pub(super) fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new(capacity))),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn new_with_session(
        capacity: usize,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new_with_session(
                capacity, session, turns,
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use codex_app_server_protocol::RequestId as AppServerRequestId;

    #[test]
    fn thread_event_store_tracks_active_turn_lifecycle() {
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        assert_eq!(store.active_turn_id(), None);

        let thread_id = ThreadId::new();
        store.push_notification(turn_started_notification(thread_id, "turn-1"));
        assert_eq!(store.active_turn_id(), Some("turn-1"));

        store.push_notification(turn_completed_notification(
            thread_id,
            "turn-2",
            TurnStatus::Completed,
        ));
        assert_eq!(store.active_turn_id(), Some("turn-1"));

        store.push_notification(turn_completed_notification(
            thread_id,
            "turn-1",
            TurnStatus::Interrupted,
        ));
        assert_eq!(store.active_turn_id(), None);
    }
    #[test]
    fn thread_event_store_restores_active_turn_from_snapshot_turns() {
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        let turns = vec![
            test_turn("turn-1", TurnStatus::Completed, Vec::new()),
            test_turn("turn-2", TurnStatus::InProgress, Vec::new()),
        ];

        let store =
            ThreadEventStore::new_with_session(/*capacity*/ 8, session.clone(), turns.clone());
        assert_eq!(store.active_turn_id(), Some("turn-2"));

        let mut refreshed_store = ThreadEventStore::new(/*capacity*/ 8);
        refreshed_store.set_session(session, turns);
        assert_eq!(refreshed_store.active_turn_id(), Some("turn-2"));
    }
    #[test]
    fn thread_event_store_clear_active_turn_id_resets_cached_turn() {
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        let thread_id = ThreadId::new();
        store.push_notification(turn_started_notification(thread_id, "turn-1"));

        store.clear_active_turn_id();

        assert_eq!(store.active_turn_id(), None);
    }
    #[test]
    fn thread_event_store_rebase_preserves_resolved_request_state() {
        let thread_id = ThreadId::new();
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        store.push_request(exec_approval_request(
            thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        ));
        store.push_notification(ServerNotification::ServerRequestResolved(
            codex_app_server_protocol::ServerRequestResolvedNotification {
                request_id: AppServerRequestId::Integer(1),
                thread_id: thread_id.to_string(),
            },
        ));

        store.rebase_buffer_after_session_refresh();

        let snapshot = store.snapshot();
        assert!(snapshot.events.is_empty());
        assert_eq!(store.has_pending_thread_approvals(), false);
    }

    #[test]
    fn thread_event_store_rebase_preserves_hook_notifications() {
        let thread_id = ThreadId::new();
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        store.push_notification(hook_started_notification(thread_id, "turn-hook"));
        store.push_notification(hook_completed_notification(thread_id, "turn-hook"));

        store.rebase_buffer_after_session_refresh();

        let snapshot = store.snapshot();
        let hook_notifications = snapshot
            .events
            .into_iter()
            .map(|event| match event {
                ThreadBufferedEvent::Notification(notification) => {
                    serde_json::to_value(notification).expect("hook notification should serialize")
                }
                other => panic!("expected buffered hook notification, saw: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            hook_notifications,
            vec![
                serde_json::to_value(hook_started_notification(thread_id, "turn-hook"))
                    .expect("hook notification should serialize"),
                serde_json::to_value(hook_completed_notification(thread_id, "turn-hook"))
                    .expect("hook notification should serialize"),
            ]
        );
    }
    #[test]
    fn thread_event_store_snapshot_carries_prompt_gc_activity_outside_event_buffer() {
        let mut store = ThreadEventStore::new(8);
        store.prompt_gc_active = true;

        let snapshot = store.snapshot();
        assert!(snapshot.events.is_empty());
        assert!(snapshot.prompt_gc_active);
        assert_eq!(snapshot.prompt_gc_token_usage_info, None);
    }
    #[test]
    fn thread_event_store_snapshot_carries_prompt_gc_token_usage_info_outside_event_buffer() {
        let mut store = ThreadEventStore::new(8);
        let token_usage_info = TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 40_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 400,
                ..TokenUsage::default()
            },
            model_context_window: Some(13_000),
        };
        store.prompt_gc_token_usage_info = Some(token_usage_info.clone());

        let snapshot = store.snapshot();
        assert!(snapshot.events.is_empty());
        assert_eq!(snapshot.prompt_gc_token_usage_info, Some(token_usage_info));
    }
    #[test]
    fn thread_event_store_clears_prompt_gc_token_usage_info_on_visible_token_count() {
        let mut store = ThreadEventStore::new(8);
        store.prompt_gc_completion_pending = true;
        store.prompt_gc_token_usage_info = Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 50_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_400,
                ..TokenUsage::default()
            },
            model_context_window: Some(13_000),
        });

        store.push_notification(token_usage_notification(
            ThreadId::new(),
            "turn-1",
            Some(13_000),
        ));

        let snapshot = store.snapshot();
        assert_eq!(snapshot.prompt_gc_token_usage_info, None);
        assert!(store.prompt_gc_private_usage_closed);
    }
    #[tokio::test]
    async fn thread_event_store_clears_prompt_gc_token_usage_info_on_turn_started() {
        let mut store = ThreadEventStore::new(8);
        store.prompt_gc_completion_pending = true;
        store.prompt_gc_private_usage_closed = true;
        store.prompt_gc_token_usage_info = Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 50_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_400,
                ..TokenUsage::default()
            },
            model_context_window: Some(13_000),
        });

        store.push_notification(turn_started_notification(ThreadId::new(), "turn-2"));

        let snapshot = store.snapshot();
        assert_eq!(snapshot.prompt_gc_token_usage_info, None);
        assert!(!store.prompt_gc_completion_pending);
        assert!(!store.prompt_gc_private_usage_closed);
    }
}
