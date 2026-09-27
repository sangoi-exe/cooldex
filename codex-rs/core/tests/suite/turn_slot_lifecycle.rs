use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use codex_rollout::RolloutItem;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::ArchiveThreadParams;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::DeleteThreadParams;
use codex_thread_store::InMemoryThreadStore;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::PersistContext;
use codex_thread_store::ReadThreadByRolloutPathParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadPage;
use codex_thread_store::ThreadStore;
use codex_thread_store::ThreadStoreFuture;
use codex_thread_store::UpdateThreadMetadataParams;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::from_slice;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

struct PendingAppend {
    params: AppendThreadItemsParams,
    complete: oneshot::Sender<()>,
}

struct GatedAppendStore {
    inner: InMemoryThreadStore,
    armed: AtomicBool,
    pending_appends: mpsc::UnboundedSender<PendingAppend>,
}

macro_rules! delegate_store_methods {
    ($(fn $name:ident($param:ident: $params:ty) -> $result:ty;)*) => {
        $(fn $name(&self, $param: $params) -> ThreadStoreFuture<'_, $result> {
            ThreadStore::$name(&self.inner, $param)
        })*
    };
}

impl ThreadStore for GatedAppendStore {
    fn as_any(&self) -> &dyn Any {
        self
    }

    delegate_store_methods! {
        fn create_thread(params: CreateThreadParams) -> ();
        fn resume_thread(params: ResumeThreadParams) -> ();
        fn discard_thread(thread_id: ThreadId) -> ();
        fn load_history(params: LoadThreadHistoryParams) -> StoredThreadHistory;
        fn read_thread(params: ReadThreadParams) -> StoredThread;
        fn read_thread_by_rollout_path(params: ReadThreadByRolloutPathParams) -> StoredThread;
        fn list_threads(params: ListThreadsParams) -> ThreadPage;
        fn archive_thread(params: ArchiveThreadParams) -> ();
        fn unarchive_thread(params: ArchiveThreadParams) -> StoredThread;
        fn delete_thread(params: DeleteThreadParams) -> ();
        fn flush_thread(thread_id: ThreadId) -> ();
        fn shutdown_thread(thread_id: ThreadId) -> ();
    }

    fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            if is_deferred_mailbox_append(&params) && self.armed.swap(false, Ordering::SeqCst) {
                let (complete, completed) = oneshot::channel();
                self.pending_appends
                    .send(PendingAppend {
                        params: params.clone(),
                        complete,
                    })
                    .expect("append receiver should remain open");
                completed.await.expect("test should complete append");
            }
            self.inner.append_items(params).await
        })
    }

    fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreFuture<'_, Option<StoredThread>> {
        self.inner.update_thread_metadata(params)
    }

    fn record_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move { self.inner.update_thread_metadata(params).await.map(|_| ()) })
    }

    fn persist_thread(
        &self,
        thread_id: ThreadId,
        context: PersistContext,
    ) -> ThreadStoreFuture<'_, ()> {
        self.inner.persist_thread(thread_id, context)
    }
}

fn is_late_mailbox_response_item(item: &RolloutItem) -> bool {
    matches!(
        item,
        RolloutItem::ResponseItem(envelope)
            if matches!(
                &envelope.item,
                ResponseItem::AgentMessage { content, .. }
                    if content.iter().any(|item| matches!(
                        item,
                        AgentMessageInputContent::InputText { text } if text == "late mailbox input"
                    ))
            )
    )
}

fn is_deferred_mailbox_append(params: &AppendThreadItemsParams) -> bool {
    params.items.iter().any(|item| {
        matches!(
            item,
            RolloutItem::InterAgentCommunicationMetadata {
                trigger_turn: false
            }
        )
    }) && params.items.iter().any(is_late_mailbox_response_item)
}

fn is_client_injected_developer_message(item: &RolloutItem, text: &str) -> bool {
    matches!(
        item,
        RolloutItem::ResponseItem(envelope)
            if matches!(
                &envelope.item,
                ResponseItem::Message { role, content, .. }
                    if role == "developer"
                        && content.iter().any(|item| matches!(
                            item,
                            ContentItem::InputText { text: item_text } if item_text == text
                        ))
            )
    )
}

fn chunk(event: Value) -> StreamingSseChunk {
    StreamingSseChunk {
        gate: None,
        body: sse(vec![event]),
    }
}

fn response_chunks(response_id: &str, message_id: &str, text: &str) -> Vec<StreamingSseChunk> {
    vec![
        chunk(ev_response_created(response_id)),
        chunk(ev_assistant_message(message_id, text)),
        chunk(ev_completed(response_id)),
    ]
}

fn message_input_texts(body: &Value, role: &str) -> Vec<String> {
    body.get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_text"))
        .filter_map(|span| span.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

async fn submit_user_input(codex: &codex_core::CodexThread, text: &str) {
    codex
        .start_or_steer_turn(codex_protocol::turn_input::TurnInputRequest::user_input(
            vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
        ))
        .await
        .expect("submit user input");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_submit_waits_for_prior_turn_terminal_transition() {
    let (first_complete, first_completion) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            chunk(ev_response_created("resp-first")),
            chunk(ev_assistant_message("msg-first", "first answer")),
            StreamingSseChunk {
                gate: Some(first_completion),
                body: sse(vec![ev_completed("resp-first")]),
            },
        ],
        response_chunks("resp-second", "msg-second", "second answer"),
    ])
    .await;
    let (pending_appends, mut append_requests) = mpsc::unbounded_channel();
    let store = Arc::new(GatedAppendStore {
        inner: InMemoryThreadStore::default(),
        armed: AtomicBool::new(false),
        pending_appends,
    });
    let codex = test_codex()
        .with_model("gpt-5.4")
        .with_thread_store(store.clone())
        .build_with_streaming_server(&server)
        .await
        .expect("build streaming Codex test session")
        .codex;

    submit_user_input(&codex, "first prompt").await;
    wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::AgentMessage(message) if message.message == "first answer"
        )
    })
    .await;
    codex
        .submit(Op::InterAgentCommunication {
            communication: codex_protocol::protocol::InterAgentCommunication::new(
                AgentPath::try_from("/root/worker").expect("worker path should parse"),
                AgentPath::root(),
                Vec::new(),
                "late mailbox input".to_string(),
                /*trigger_turn*/ false,
            ),
            start_options: Default::default(),
        })
        .await
        .expect("submit queue-only mailbox input");
    store.armed.store(true, Ordering::SeqCst);
    first_complete
        .send(())
        .expect("first response completion should remain gated");
    // Completion persists deferred mailbox mail after the active slot enters terminal
    // finalization, before terminal lifecycle delivery can emit TurnComplete.
    let pending_append = tokio::time::timeout(Duration::from_secs(2), append_requests.recv())
        .await
        .expect("terminal transition should persist queued mail")
        .expect("append observer should remain open");
    assert!(
        is_deferred_mailbox_append(&pending_append.params),
        "gate must pause the deferred queue-only mailbox append"
    );

    let second_submit = tokio::spawn({
        let codex = Arc::clone(&codex);
        async move {
            submit_user_input(&codex, "second prompt").await;
        }
    });
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            server.wait_for_request_count(/*count*/ 2),
        )
        .await
        .is_err(),
        "fresh submit must not start while the prior turn is transitioning"
    );

    pending_append
        .complete
        .send(())
        .expect("terminal append should still be waiting");
    second_submit
        .await
        .expect("second submit task should finish after the transition");
    let mut lifecycle = Vec::new();
    while lifecycle.len() < 2 {
        let event = tokio::time::timeout(Duration::from_secs(2), codex.next_event())
            .await
            .expect("lifecycle event should arrive")
            .expect("event channel should remain open");
        match event.msg {
            EventMsg::TurnComplete(event) => {
                lifecycle.push(("complete", event.turn_id));
            }
            EventMsg::TurnStarted(event) => {
                lifecycle.push(("started", event.turn_id));
            }
            _ => {}
        }
    }
    assert_eq!(lifecycle[0].0, "complete");
    assert_eq!(lifecycle[1].0, "started");
    assert_ne!(lifecycle[0].1, lifecycle[1].1);

    wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::AgentMessage(message) if message.message == "second answer"
        )
    })
    .await;
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    server.wait_for_request_count(/*count*/ 2).await;
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let first_request: Value = from_slice(&requests[0]).expect("parse first request");
    let second_request: Value = from_slice(&requests[1]).expect("parse second request");
    assert!(
        !message_input_texts(&first_request, "user")
            .iter()
            .any(|text| text == "second prompt")
    );
    assert_eq!(
        message_input_texts(&second_request, "user")
            .iter()
            .filter(|text| text.as_str() == "second prompt")
            .count(),
        1
    );
    assert_eq!(
        second_request["input"]
            .as_array()
            .expect("second request input")
            .iter()
            .filter(|item| {
                item["type"] == "agent_message"
                    && item["content"].as_array().is_some_and(|content| {
                        content.iter().any(|content| {
                            content["type"] == "input_text"
                                && content["text"] == "late mailbox input"
                        })
                    })
            })
            .count(),
        1
    );
    let history = codex
        .load_history(/*include_archived*/ false)
        .await
        .expect("load thread history");
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| is_late_mailbox_response_item(item))
            .count(),
        1
    );
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    RolloutItem::InterAgentCommunicationMetadata {
                        trigger_turn: false
                    }
                )
            })
            .count(),
        1
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_waits_for_prior_turn_terminal_finalization() {
    let (first_complete, first_completion) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            chunk(ev_response_created("resp-first")),
            chunk(ev_assistant_message("msg-first", "first answer")),
            StreamingSseChunk {
                gate: Some(first_completion),
                body: sse(vec![ev_completed("resp-first")]),
            },
        ],
        vec![
            chunk(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "terminal lifecycle compaction",
                }
            })),
            chunk(ev_completed("resp-compact")),
        ],
    ])
    .await;
    let (pending_appends, mut append_requests) = mpsc::unbounded_channel();
    let store = Arc::new(GatedAppendStore {
        inner: InMemoryThreadStore::default(),
        armed: AtomicBool::new(false),
        pending_appends,
    });
    let codex = test_codex()
        .with_model("gpt-5.4")
        .with_thread_store(store.clone())
        .build_with_streaming_server(&server)
        .await
        .expect("build streaming Codex test session")
        .codex;

    submit_user_input(&codex, "first prompt").await;
    wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::AgentMessage(message) if message.message == "first answer"
        )
    })
    .await;
    codex
        .submit(Op::InterAgentCommunication {
            communication: codex_protocol::protocol::InterAgentCommunication::new(
                AgentPath::try_from("/root/worker").expect("worker path should parse"),
                AgentPath::root(),
                Vec::new(),
                "late mailbox input".to_string(),
                /*trigger_turn*/ false,
            ),
            start_options: Default::default(),
        })
        .await
        .expect("submit queue-only mailbox input");
    store.armed.store(true, Ordering::SeqCst);
    first_complete
        .send(())
        .expect("first response completion should remain gated");
    let pending_append = tokio::time::timeout(Duration::from_secs(2), append_requests.recv())
        .await
        .expect("terminal transition should persist queued mail")
        .expect("append observer should remain open");
    assert!(
        is_deferred_mailbox_append(&pending_append.params),
        "gate must pause the deferred queue-only mailbox append"
    );

    codex.submit(Op::Compact).await.expect("submit compact");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            server.wait_for_request_count(/*count*/ 2),
        )
        .await
        .is_err(),
        "compact must not start while the prior turn is finalizing"
    );

    pending_append
        .complete
        .send(())
        .expect("terminal append should still be waiting");
    let EventMsg::TurnComplete(first_complete) =
        wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await
    else {
        unreachable!("predicate guarantees a turn completion event");
    };
    assert_eq!(
        first_complete.last_agent_message.as_deref(),
        Some("first answer")
    );
    server.wait_for_request_count(/*count*/ 2).await;
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_injection_waits_for_terminal_finalization_and_persists_once() {
    let (first_complete, first_completion) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![vec![
        chunk(ev_response_created("resp-first")),
        chunk(ev_assistant_message("msg-first", "first answer")),
        StreamingSseChunk {
            gate: Some(first_completion),
            body: sse(vec![ev_completed("resp-first")]),
        },
    ]])
    .await;
    let (pending_appends, mut append_requests) = mpsc::unbounded_channel();
    let store = Arc::new(GatedAppendStore {
        inner: InMemoryThreadStore::default(),
        armed: AtomicBool::new(false),
        pending_appends,
    });
    let codex = test_codex()
        .with_model("gpt-5.4")
        .with_thread_store(store.clone())
        .build_with_streaming_server(&server)
        .await
        .expect("build streaming Codex test session")
        .codex;

    submit_user_input(&codex, "first prompt").await;
    wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::AgentMessage(message) if message.message == "first answer"
        )
    })
    .await;
    codex
        .submit(Op::InterAgentCommunication {
            communication: codex_protocol::protocol::InterAgentCommunication::new(
                AgentPath::try_from("/root/worker").expect("worker path should parse"),
                AgentPath::root(),
                Vec::new(),
                "late mailbox input".to_string(),
                /*trigger_turn*/ false,
            ),
            start_options: Default::default(),
        })
        .await
        .expect("submit queue-only mailbox input");
    store.armed.store(true, Ordering::SeqCst);
    first_complete
        .send(())
        .expect("first response completion should remain gated");
    let pending_append = tokio::time::timeout(Duration::from_secs(2), append_requests.recv())
        .await
        .expect("terminal transition should persist queued mail")
        .expect("append observer should remain open");
    assert!(
        is_deferred_mailbox_append(&pending_append.params),
        "gate must pause the deferred queue-only mailbox append"
    );

    let injected_text = "client response item during terminal finalization";
    let injected_item = serde_json::from_value(json!({
        "type": "message",
        "role": "developer",
        "content": [{"type": "input_text", "text": injected_text}],
    }))
    .expect("parse injected response item");
    let mut injection = tokio::spawn({
        let codex = Arc::clone(&codex);
        async move { codex.inject_response_items(vec![injected_item]).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut injection)
            .await
            .is_err(),
        "client injection must wait for the finishing task to retire"
    );

    pending_append
        .complete
        .send(())
        .expect("terminal append should still be waiting");
    tokio::time::timeout(Duration::from_secs(2), injection)
        .await
        .expect("client injection should finish after terminal retirement")
        .expect("client injection task should not panic")
        .expect("client injection should succeed");
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let history = codex
        .load_history(/*include_archived*/ false)
        .await
        .expect("load thread history");
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| is_client_injected_developer_message(item, injected_text))
            .count(),
        1,
        "the client-injected response item must remain durable exactly once"
    );

    server.shutdown().await;
}
