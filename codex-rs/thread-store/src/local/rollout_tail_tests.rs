use std::fs;
use std::fs::OpenOptions;
use std::io::Write;

use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::ThreadHistoryMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::ThreadStore;
use crate::local::test_support::test_config;
use crate::local::test_support::write_session_file_with_history_mode;

fn message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn append_line(path: &std::path::Path, ordinal: u64, item: RolloutItem) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open rollout");
    let line = RolloutLine {
        timestamp: "2025-01-03T13:00:00Z".to_string(),
        ordinal: Some(ordinal),
        item,
    };
    writeln!(
        file,
        "{}",
        serde_json::to_string(&line).expect("serialize rollout line")
    )
    .expect("append rollout line");
}

#[tokio::test]
async fn loads_complete_rollout_in_replay_order() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3001);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message("first"));
    append_line(path.as_path(), 2, message("second"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("load rollout tail");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual tail"),
        serde_json::to_value(vec![message("first"), message("second")])
            .expect("serialize expected tail")
    );
    assert!(tail.reached_start);
    assert_eq!(tail.records_read, 3);
    assert_eq!(tail.segments_read, 1);
    assert!(tail.bytes_read > 0);
}

#[tokio::test]
async fn loads_frozen_parent_lineage_before_the_child_delta() {
    let home = TempDir::new().expect("temp dir");
    let parent_uuid = Uuid::from_u128(/*v*/ 3010);
    let parent_id = ThreadId::from_string(&parent_uuid.to_string()).expect("parent id");
    let parent_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-03-00",
        parent_uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write parent rollout");
    append_line(parent_path.as_path(), 1, message("parent first"));
    append_line(parent_path.as_path(), 2, message("parent frozen"));
    let parent_end = HistoryPosition {
        thread_id: parent_id,
        end_ordinal_exclusive: 3,
        end_byte_offset: fs::metadata(parent_path.as_path())
            .expect("parent metadata")
            .len(),
    };

    let child_uuid = Uuid::from_u128(/*v*/ 3011);
    let child_id = ThreadId::from_string(&child_uuid.to_string()).expect("child id");
    let child_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-03-01",
        child_uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write child rollout");
    set_history_base(child_path.as_path(), parent_end);
    append_line(child_path.as_path(), 1, message("child delta"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id: child_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("load fork lineage");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual lineage"),
        serde_json::to_value(vec![
            message("parent first"),
            message("parent frozen"),
            message("child delta"),
        ])
        .expect("serialize expected lineage")
    );
    assert!(tail.reached_start);
    assert_eq!(tail.records_read, 5);
    assert_eq!(tail.segments_read, 2);
}

#[tokio::test]
async fn reports_byte_limited_incomplete_source_without_parsing_a_partial_record() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3002);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-01",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message(&"x".repeat(256)));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 32,
            max_records: 16,
        })
        .await
        .expect("bounded partial read");

    assert!(tail.items.is_empty());
    assert!(!tail.reached_start);
    assert_eq!(tail.bytes_read, 32);
    assert_eq!(tail.records_read, 0);
    assert_eq!(tail.segments_read, 1);
}

#[tokio::test]
async fn reports_record_limited_newest_complete_record() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3004);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-03",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message("older"));
    append_line(path.as_path(), 2, message("newest"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 1,
        })
        .await
        .expect("record-bounded tail");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual tail"),
        serde_json::to_value(vec![message("newest")]).expect("serialize expected tail")
    );
    assert!(!tail.reached_start);
    assert_eq!(tail.records_read, 1);
    assert_eq!(tail.segments_read, 1);
}

#[tokio::test]
async fn rejects_malformed_rollout_records() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3003);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-02",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open rollout");
    writeln!(file, "not-json").expect("append malformed line");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let err = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect_err("malformed record should fail");

    assert!(matches!(err, ThreadStoreError::Internal { .. }));
    assert!(err.to_string().contains("rejected rollout record"));
}

fn set_history_base(path: &std::path::Path, history_base: HistoryPosition) {
    let contents = fs::read_to_string(path).expect("read child rollout");
    let (head, tail) = contents.split_once('\n').expect("session metadata line");
    let mut line: RolloutLine = serde_json::from_str(head).expect("parse session metadata line");
    let RolloutItem::SessionMeta(session_meta) = &mut line.item else {
        panic!("expected session metadata head");
    };
    session_meta.meta.history_base = Some(history_base);
    fs::write(
        path,
        format!(
            "{}\n{tail}",
            serde_json::to_string(&line).expect("serialize session metadata line")
        ),
    )
    .expect("rewrite child metadata");
}
